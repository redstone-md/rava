//! Minimal-but-correct `.class` parser. Today it resolves names, the constant
//! pool, fields and methods (with raw attribute bodies). The bytecode
//! interpreter will grow on top of this foundation — nothing here is throwaway.

mod reader;
mod pool;
mod code;
mod members;
pub mod version;

pub use pool::{unmap_surrogate, Constant, ConstantPool, MemberRef};
pub use code::{Code, ExceptionHandler};

use members::MethodIndex;
use reader::Reader;

const MAGIC: u32 = 0xCAFE_BABE;

/// Dependency-free FxHash for the method-name lookup, which is consulted on
/// every method call. SipHash (the std default) is DoS-resistant — wasted on a
/// trusted in-process name map — and its per-lookup cost shows up on
/// dispatch-heavy boots.
#[derive(Default)]
pub struct FxHasher {
    hash: u64,
}
const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;
impl std::hash::Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.hash = (self.hash.rotate_left(5) ^ b as u64).wrapping_mul(FX_SEED);
        }
    }
}
type FastMap<K, V> = std::collections::HashMap<K, V, std::hash::BuildHasherDefault<FxHasher>>;

/// Kill switches this crate reads for itself.
///
/// DUPLICATED, not imported: `rava-interp` depends on `rava-classfile`, so calling its
/// `flags::emit_more_disabled()` from here would be a dependency cycle. The env var is the
/// same one, memoised the same way, so a single same-binary A/B against `RAVA_NO_EMIT_MORE`
/// still prices this change together with the rest of the block — which is the point of that
/// switch existing.
pub mod flags {
    /// `RAVA_NO_FAST_CLASSFILE=1` (or the block switch `RAVA_NO_EMIT_MORE=1`) restores the
    /// allocating `Box<str>`-keyed method map and the eager annotation decode. DEFAULT ON.
    ///
    /// MEMOISED: on Windows `var_os` is `RtlQueryEnvironmentVariable` — a linear walk of the
    /// environment block plus an allocation — and the answer is consulted once per class on the
    /// class-loading path, which runs ~15 000 times a boot. The only writer of the environment
    /// is the CLI's config apply, which completes before any class is parsed.
    #[inline]
    pub fn fast_classfile() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| {
            // ⭐**PROMOTED OUT OF THE ROUND-3 BLOCK, WHICH WAS SUPPRESSING A REAL WIN.**
            //
            // This fix shipped inside a seven-area block that measured ~3% SLOWER as a whole and
            // was therefore demoted to `RAVA_FAST_NATIVES=1`. Isolated — the block enabled but its
            // other six areas switched off — it prices as its own arm, ABBA, three boots each:
            //
            //     classfile only   38.1 / 37.7 / 37.8   median 37.8   min 37.7
            //     base             38.8 / 38.4 / 38.3   median 38.4   min 38.3
            //                                           -1.6%, and the arms are DISJOINT
            //
            // ⇒ A block verdict is not a verdict on its members. The parser runs once per class
            // for ~15,000 classes and never benefits from a JIT, so removing ~1.5 M `Box<str>`
            // duplications of names the `ClassFile` already owns is removed from 53% of the wall.
            std::env::var_os("RAVA_NO_FAST_CLASSFILE").is_none()
                && std::env::var_os("RAVA_NO_EMIT_MORE").is_none()
        })
    }

    /// `RAVA_NO_FAST_CFMETHOD=1` (or the block switch `RAVA_NO_EMIT_MORE=1`) restores the
    /// LENGTH-PROPORTIONAL member hash and the `memcmp`-libcall verification inside
    /// [`crate::ClassFile::method`]. DEFAULT ON.
    ///
    /// Separate from `fast_classfile` above on purpose: that switch swaps the whole index
    /// STRUCTURE (open-addressed table vs the `Box<str>`-keyed map), this one swaps only how a
    /// key is folded and compared inside the table. One A/B, one variable.
    ///
    /// MEMOISED for the same reason, and folded once into `member_key_mode` — a
    /// process constant, because a table built with one fold and probed with another degenerates
    /// into a full walk. `RAVA_NO_SIMD_CP=1` turns the same mechanism off on its own.
    #[inline]
    pub fn fast_cfmethod() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| {
            std::env::var_os("RAVA_NO_FAST_CFMETHOD").is_none()
                && std::env::var_os("RAVA_NO_EMIT_MORE").is_none()
        })
    }

    /// `RAVA_NO_FAST_CFMETHOD2=1` (or the block switch `RAVA_NO_EMIT_MORE=1`) stops
    /// [`crate::ClassFile`] from recording the POOL INDEX PAIR each method was parsed from, which
    /// in turn makes [`crate::ClassFile::method_nat`] degenerate to exactly the string lookup it
    /// wraps. DEFAULT ON.
    ///
    /// ⭐**READ ONCE PER CLASS, AT PARSE, NEVER PER LOOKUP.** The switch removes the key array
    /// itself, and every lookup-side test is then a LENGTH test against that array — the same
    /// lesson `MethodIndex::Hashed` learned when it moved the fold mode inside the table it built:
    /// a `OnceLock` acquire on the probe path is a barrier LLVM may not hoist, and this probe is
    /// three instructions long. So the A/B lever removes one allocation per class AND the scan,
    /// which is the whole mechanism, in one variable.
    ///
    /// MEMOISED for the same reason the two switches above are: on Windows `var_os` walks the
    /// environment block and allocates, and this is asked once per parsed class (~15 000 a boot).
    #[inline]
    pub fn fast_cfmethod2() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| {
            std::env::var_os("RAVA_NO_FAST_CFMETHOD2").is_none()
                && std::env::var_os("RAVA_NO_EMIT_MORE").is_none()
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// THE POOL-INDEX KEY — why a method lookup does not need to touch a string at all.
//
// `ClassFile::method(name, descriptor)` is 1.38% of the callback profile, and by now
// essentially all of that is SHAPE, not code: the fold is one `crc32q` chain and the verify is
// an inline SSE2 compare (see `crate::members`). What is left is the three cache lines a hit
// touches — the `MemberInfo` (unavoidable, it is the return value) plus the two `String`
// bodies the verify has to read — and the ~45 bytes of name+descriptor the fold has to eat.
//
// But the caller almost never HAS two strings. It has a `CONSTANT_NameAndType`, and it turned
// that into two `&str` by two pool loads purely so this crate could turn them back into a hash.
// A `NameAndType` is already a 32-bit identity: `(name_index, desc_index)`. Inside ONE constant
// pool that pair identifies the exact Utf8 bytes, so:
//
//   FOLD    nothing at all — the key IS the u32.
//   VERIFY  a 32-bit integer compare, so no `String` body is read: 3 cache lines -> 1 (the
//           key array) plus the `MemberInfo` the caller wanted anyway.
//
// ⚠**AND THE POOL IS PART OF THE KEY.** Index 7 of one class's pool and index 7 of another's
// are unrelated bytes, so a pool index probed against the wrong class's key array can MATCH and
// return a method with a completely different name. That is the word-identity trap in its
// purest form, so the pool is not documentation here, it is an ARGUMENT: `method_nat` takes the
// `&ConstantPool` the indices came from and compares it by address with its own. A super-class
// walk — caller's pool, callee's class — fails that test on the first ancestor and takes the
// cold arm, which is byte-for-byte the lookup that runs today.
//
// EXACTNESS. `parse_members` records `(name_index, desc_index)` ONLY when both resolved through
// `pool.utf8`, i.e. exactly when `methods[i].name` and `.descriptor` are copies of those two
// entries; a single failure poisons the whole class's array (it is left empty and every lookup
// takes the cold arm), so the invariant needs no per-slot validity bit:
//
//     key[i] == pack(n, d)  =>  pool.utf8(n) == Some(&methods[i].name)
//                           and pool.utf8(d) == Some(&methods[i].descriptor)
//
// ⇒ a key hit returns a method whose name and descriptor are byte-equal to what the string
// lookup was asked for. A key MISS is NOT proof of absence — a pool may legally hold two Utf8
// entries spelling the same text — so a miss always falls through to the string lookup.
//
// ⛔THE ONE DOCUMENTED DIFFERENCE, and it needs an ILLEGAL class file to observe: if two
// methods share a (name, descriptor) — forbidden by JVMS 4.6, not rejected here — the string
// table returns the LOWEST index and the key scan returns the lowest index carrying THOSE
// indices, which may be the second one if the two members were written with different Utf8
// slots. Both answers have the right name and the right descriptor; JVMS defines neither.
// `RAVA_NO_FAST_CFMETHOD2=1` restores the string table's choice.
// ─────────────────────────────────────────────────────────────────────────────

/// Pack a `CONSTANT_NameAndType`'s two halves into the lookup key.
///
/// Name in the HIGH half so the array stays sorted-ish by name for a class whose javac emitted
/// its Utf8 entries in declaration order — irrelevant to correctness, mildly kind to the branch
/// predictor on the scan below.
#[inline]
const fn pack_nat(name_index: u16, desc_index: u16) -> u32 {
    ((name_index as u32) << 16) | desc_index as u32
}

/// Above this many methods the linear scan stops being the cheap answer and the caller is sent
/// to the (already good) hashed string table instead.
///
/// THE ARITHMETIC, not a taste: 64 keys is 256 bytes = **4 cache lines**, and the SSE2 form
/// below eats 4 keys per iteration, so a miss costs 16 compares and a hit averages 8. Against
/// that, the string path folds ~45 bytes of name+descriptor through `crc32q` and then reads two
/// `String` bodies to verify — comparable cycles and strictly more cache lines. Past 64 the scan
/// loses on both counts, and a 300-method class is exactly where an open-addressed table earns
/// its allocation, so the cap is a HANDOFF, not a cliff: over it, `method_nat` is the string
/// lookup with one extra length test.
const NAT_SCAN_MAX: usize = 64;

/// First index in `keys` holding `key`, or `None`.
///
/// COVERAGE — why this is `keys.iter().position(|&k| k == key)` exactly:
///  * lengths under 4 go to the scalar form verbatim;
///  * the indexed 4-lane chunks cover `[0, 4*floor(n/4))` IN ORDER, and within a chunk the
///    lowest set mask bit is the lowest lane, so the first match reported is the lowest index;
///  * the final OVERLAPPING load covers `[n-4, n)` and its mask is then AND-ed clear of the
///    `i - base` lanes the loop already examined, so a byte already rejected cannot be reported
///    and the answer is again the lowest surviving index.
///
/// NEVER READS PAST THE END: every load offset is `i` with `i + 4 <= n`, or exactly `n - 4`
/// with `n >= 4`. No 16-byte load is issued for a shorter slice.
#[cfg(target_arch = "x86_64")]
#[inline]
fn nat_scan(keys: &[u32], key: u32) -> Option<usize> {
    use core::arch::x86_64::{
        __m128i, _mm_cmpeq_epi32, _mm_loadu_si128, _mm_movemask_epi8, _mm_set1_epi32,
    };
    let n = keys.len();
    if n < 4 {
        return nat_scan_scalar(keys, key);
    }
    let p = keys.as_ptr();
    // SAFETY: see the coverage note above — every load lies inside `keys`. SSE2 is baseline on
    // x86_64, so no feature probe is needed (unlike the `crc32q` fold in `crate::members`).
    unsafe {
        let want = _mm_set1_epi32(key as i32);
        let mut i = 0usize;
        while i + 4 <= n {
            let v = _mm_loadu_si128(p.add(i) as *const __m128i);
            // `movemask_epi8` gives FOUR bits per 32-bit lane, hence the `>> 2`.
            let m = _mm_movemask_epi8(_mm_cmpeq_epi32(v, want)) as u32;
            if m != 0 {
                return Some(i + (m.trailing_zeros() as usize >> 2));
            }
            i += 4;
        }
        if i < n {
            // `i` is a multiple of 4 and `i < n`, so `n % 4 != 0` and `i - base` is 1..=3.
            let base = n - 4;
            let v = _mm_loadu_si128(p.add(base) as *const __m128i);
            let skip = ((i - base) * 4) as u32;
            let m = (_mm_movemask_epi8(_mm_cmpeq_epi32(v, want)) as u32) & !((1u32 << skip) - 1);
            if m != 0 {
                return Some(base + (m.trailing_zeros() as usize >> 2));
            }
        }
        None
    }
}

/// The portable form, and the definition the vector form above is proved against.
#[cfg(not(target_arch = "x86_64"))]
#[inline]
fn nat_scan(keys: &[u32], key: u32) -> Option<usize> {
    nat_scan_scalar(keys, key)
}

#[inline]
fn nat_scan_scalar(keys: &[u32], key: u32) -> Option<usize> {
    keys.iter().position(|&k| k == key)
}


#[derive(Debug)]
pub enum ClassError {
    UnexpectedEof,
    BadMagic(u32),
    BadConstantTag(u8),
    MissingName,
}

impl std::fmt::Display for ClassError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClassError::UnexpectedEof => write!(f, "unexpected end of class file"),
            ClassError::BadMagic(m) => write!(f, "bad magic 0x{m:08X} (not 0xCAFEBABE)"),
            ClassError::BadConstantTag(t) => write!(f, "unknown constant pool tag {t}"),
            ClassError::MissingName => write!(f, "this_class did not resolve to a name"),
        }
    }
}

impl std::error::Error for ClassError {}

/// One field or method: `{flags, name, desc, attributes}` (JVMS 4.5 / 4.6).
///
/// ⛔**`name` AND `descriptor` ARE COPIES OF POOL TEXT THIS CLASS ALREADY OWNS AND OUTLIVES** —
/// two `String` allocations per member, ~60 members per class, ~15 000 classes, so **~1.8 M
/// allocations a boot** whose only job is to duplicate bytes sitting one `pool.utf8(idx)` away.
/// The parser never benefits from a JIT, so every one of them is charged to the 53% of the wall
/// that runs before `Environment:`.
///
/// ⛔**AND IT DOES NOT COME OUT IN HALVES. Measured before writing any of it:**
///
///  1. **Adding the pool indices here is NOT the "non-breaking half".** A Rust struct literal
///     must name every field, so a new field breaks construction just as surely as a removed
///     one does. `rava-interp` builds `MemberInfo` by literal in three places
///     (`fork_batch_tests/mod.rs:26`, `fork_batch_tests/fork1.rs:199` and `:200`); all three are
///     `#[cfg(test)]`, so `cargo build` would stay green and `cargo test` would not — the worst
///     possible failure mode in a tree several agents share and that is built once.
///  2. **And the indices would have no reader.** Routing the verify in `members::lookup` through
///     the pool while these `String`s still exist makes it STRICTLY SLOWER, not faster: today a
///     hit touches the `MemberInfo` (needed anyway, it is the return value) plus the two string
///     bodies = **3 cache lines**; through the pool it is `entries[name_index]`, its body,
///     `entries[desc_index]`, its body, PLUS the `MemberInfo` = **5**. That is the shape this
///     engine has already demoted twice — a mirror added in front of a path that was cheap.
///     The redirect only pays once the `String`s are GONE, at which point it is 3 lines again
///     and 1.8 M allocations lighter.
///  3. **The removal is not a round, it is a refactor.** `rava-interp` reads `.name`/`.descriptor`
///     off a member at ~334 sites in 60+ files (`stacktrace.rs`, `npe.rs`, `prof.rs`,
///     `reflect/members.rs`, the whole `jit/emit/ops/invoke_*` family, `vm/dispatch.rs`, …), and
///     most of them hold a bare `&MemberInfo` with no `&ConstantPool` in reach. On top of that
///     `ClassFile::synthesize` has five callers that mint members with names that are in NO pool
///     (the fork compiler and the jit tests), so a pool-backed `MemberInfo` needs a synthetic
///     pool to be interned into first.
///
/// ⇒ **The order is: (a) give every `synthesize` caller a pool, (b) flip the ~334 reads to
/// accessors that take `&ConstantPool`, (c) THEN swap these two fields for `u16` indices and
/// point `members::lookup` at the pool.** Steps (a)+(b) are mechanical and bit-exact; only (c)
/// changes what is allocated. Doing (c)'s bookkeeping first buys nothing and cannot be measured.
#[derive(Debug, Clone)]
pub struct MemberInfo {
    pub access_flags: u16,
    pub name: String,
    pub descriptor: String,
    /// Present for non-abstract, non-native methods.
    pub code: Option<Code>,
}

/// One `BootstrapMethods` entry (JVMS 4.7.23): the `MethodHandle` constant that
/// links an `invokedynamic`, plus its static argument constant-pool indices.
#[derive(Debug, Clone)]
pub struct BootstrapMethod {
    pub handle_index: u16,
    pub args: Vec<u16>,
}

pub struct ClassFile {
    pub minor: u16,
    pub major: u16,
    pub pool: ConstantPool,
    pub access_flags: u16,
    pub this_class: String,
    pub super_class: Option<String>,
    pub interfaces: Vec<String>,
    pub fields: Vec<MemberInfo>,
    pub methods: Vec<MemberInfo>,
    pub bootstrap_methods: Vec<BootstrapMethod>,
    /// Generic `Signature` attribute (JVMS 4.7.9), when present — e.g. an
    /// anonymous `new TypeToken<List<X>>(){}` subclass records its parameterized
    /// superclass here. Needed for `Class.getGenericSuperclass`.
    pub signature: Option<String>,
    /// Class-level `RuntimeVisibleAnnotations` type descriptors (e.g.
    /// `Lio/netty/channel/ChannelHandler$Sharable;`), for `Class.isAnnotationPresent`.
    /// netty rejects re-adding a non-@Sharable handler, so this must be accurate.
    pub annotations: Vec<String>,
    /// Lazy method index, built on first `method()` call. Replaces the per-call linear scan
    /// over `methods` (classes can have dozens of methods; resolution is a hot interpreter
    /// path — `ClassFile::method` alone is 0.91% of the compiled profile).
    ///
    /// ⛔**It used to be a `FastMap<Box<str>, Vec<(Box<str>, usize)>>`, and that map COPIED
    /// strings the `ClassFile` already owns and outlives**: a `Box<str>` of every method name,
    /// a `Vec` per distinct name, and a `Box<str>` of every descriptor — about three heap
    /// allocations per method, ~1.5 M of them across a 15 000-class boot, purely to duplicate
    /// bytes sitting a pointer away in `self.methods`. The parser never gets faster from a JIT,
    /// so that allocation was charged straight to the 53% of the boot that is one-shot work.
    ///
    /// Now the index stores no strings at all: `(hash(name, descriptor), method index)` pairs
    /// in one open-addressed table, and a hit is VERIFIED against `self.methods[i]` — so the
    /// hash is a pure accelerator and cannot change which method is found. The table also
    /// carries the FOLD it was built with, so probing it can never consult a different one; the
    /// whole mechanism lives in [`crate::members`].
    ///
    /// `sync::OnceLock`, not `cell::OnceCell`: `ClassFile` is embedded in `LoadedClass`, so a
    /// `!Sync` field here makes `Arc<LoadedClass>` `!Send` just as surely as one declared there
    /// - which is how this outlived three rounds of "the last blocker is X". Enumerating a
    /// struct's OWN fields misses the ones it embeds; only `assert_send::<InlineCallee>()` in
    /// `rava-interp`'s jit tests actually checks the claim.
    ///
    /// Same access pattern either way (set once, read many), and the value is a pure function of
    /// the immutable method table, so a race just recomputes an identical map.
    method_lookup: std::sync::OnceLock<MethodIndex>,
    /// Parallel to `methods`: for method `i`, the `(name_index, desc_index)` of THIS class's
    /// pool it was parsed from, packed by [`pack_nat`]. The key array of the index-keyed lookup
    /// — see the block comment above [`pack_nat`] for what it buys and what it may not claim.
    ///
    /// ⭐**IT IS EMPTY WHENEVER IT CANNOT BE TRUSTED, AND THAT IS THE ONLY GUARD ON THE PROBE
    /// PATH.** Empty when: the class was `synthesize`d (its members have no pool text behind
    /// them), a member's name or descriptor did not resolve through `pool.utf8` (a malformed
    /// class file — the whole array is dropped rather than carrying a per-slot validity bit),
    /// or `RAVA_NO_FAST_CFMETHOD2=1`. So `method_nat`'s test is `keys.len() == methods.len()`,
    /// a length compare against a slice it is about to read — no atomic, no `OnceLock`, no env
    /// read on a path taken millions of times.
    ///
    /// COST, COUNTED: 4 bytes per method in ONE `reserve_exact`-sized allocation per class.
    /// A 60-method class is 240 bytes; ~15 000 classes is **~3.6 MB retained and ~15 000
    /// allocations added against the ~1.8 M this parser already performs** (+0.8% on a row that
    /// is 4.04% of the callback profile, i.e. ~0.03%). Built from indices `parse_members` has
    /// already loaded into registers, so the fill itself is two shifts and a `push`.
    ///
    /// NOT a `OnceLock`: there is nothing to defer. The value is known at parse time and
    /// deriving it later would mean searching the pool for each method's name, which is the
    /// "mirror in front of a path that was cheap" this engine has demoted twice.
    method_nat_keys: Box<[u32]>,
}

impl ClassFile {
    pub fn parse(bytes: &[u8]) -> Result<Self, ClassError> {
        let mut r = Reader::new(bytes);

        let magic = r.u32()?;
        if magic != MAGIC {
            return Err(ClassError::BadMagic(magic));
        }
        let minor = r.u16()?;
        let major = r.u16()?;

        let cp_count = r.u16()?;
        let pool = ConstantPool::parse(&mut r, cp_count)?;

        let access_flags = r.u16()?;
        let this_index = r.u16()?;
        let this_class = pool
            .class_name(this_index)
            .ok_or(ClassError::MissingName)?
            .to_string();

        let super_index = r.u16()?;
        // super_class index 0 means java/lang/Object's own (only Object).
        let super_class = if super_index == 0 {
            None
        } else {
            pool.class_name(super_index).map(str::to_string)
        };

        let interface_count = r.u16()?;
        let mut interfaces = Vec::with_capacity(interface_count as usize);
        for _ in 0..interface_count {
            let idx = r.u16()?;
            if let Some(name) = pool.class_name(idx) {
                interfaces.push(name.to_string());
            }
        }

        // FIELDS GET NO KEY ARRAY: nothing looks a field up by pool index, and `None` here means
        // the `reserve_exact` + `push` below cost a field-heavy class literally nothing.
        let fields = Self::parse_members(&mut r, &pool, None)?;
        let mut nat_keys: Vec<u32> = Vec::new();
        let collect = flags::fast_cfmethod2();
        let methods = Self::parse_members(&mut r, &pool, collect.then_some(&mut nat_keys))?;
        // The length test IS the poison check: `parse_members` clears the vector and stops
        // collecting the moment a name or descriptor fails to resolve, so a short array can only
        // mean "this class's keys are not trustworthy" — and an empty `Vec` boxes without
        // allocating, so the disabled and malformed cases cost nothing.
        let method_nat_keys: Box<[u32]> = if collect && nat_keys.len() == methods.len() {
            nat_keys.into_boxed_slice()
        } else {
            Box::default()
        };
        let (bootstrap_methods, signature, annotations) = Self::parse_class_attributes(&mut r, &pool)?;

        Ok(ClassFile {
            minor,
            major,
            pool,
            access_flags,
            this_class,
            super_class,
            interfaces,
            fields,
            methods,
            bootstrap_methods,
            signature,
            annotations,
            method_lookup: std::sync::OnceLock::new(),
            method_nat_keys,
        })
    }

    /// Construct a class WITHOUT parsing — for tests and tooling that synthesize
    /// bytecode (mirrors `ConstantPool::from_entries`). Every parsed-only field
    /// defaults to empty; behaviour of the result is identical to a parsed class
    /// with the same members.
    pub fn synthesize(
        pool: ConstantPool,
        access_flags: u16,
        this_class: String,
        super_class: Option<String>,
        interfaces: Vec<String>,
        methods: Vec<MemberInfo>,
    ) -> Self {
        ClassFile {
            minor: 0,
            major: 52,
            pool,
            access_flags,
            this_class,
            super_class,
            interfaces,
            fields: Vec::new(),
            methods,
            bootstrap_methods: Vec::new(),
            signature: None,
            annotations: Vec::new(),
            method_lookup: std::sync::OnceLock::new(),
            // NO KEY ARRAY, DELIBERATELY. A synthesized member's `name`/`descriptor` are minted
            // by the caller (the fork compiler, the jit tests) and are in NO pool, so there is no
            // index pair that identifies them and `method_nat` must take its cold arm here. The
            // empty `Box` says exactly that, and allocates nothing.
            method_nat_keys: Box::default(),
        }
    }

    /// Walk the class-level `attributes` table, decoding `BootstrapMethods`
    /// (needed for `invokedynamic`) and `Signature` (generic superclass for
    /// reflection), skipping the rest.
    fn parse_class_attributes(
        r: &mut Reader,
        pool: &ConstantPool,
    ) -> Result<(Vec<BootstrapMethod>, Option<String>, Vec<String>), ClassError> {
        let count = r.u16()?;
        let mut bootstrap = Vec::new();
        let mut signature = None;
        let mut annotations = Vec::new();
        for _ in 0..count {
            let name_index = r.u16()?;
            let length = r.u32()? as usize;
            match pool.utf8(name_index) {
                Some("BootstrapMethods") => {
                    let n = r.u16()?;
                    // The count is in the header, so size the vector once instead of letting
                    // `push` double it 1 -> 2 -> 4 -> ... A lambda-heavy class carries dozens
                    // of bootstrap methods and this attribute is parsed once per class, 15 000
                    // times a boot.
                    bootstrap.reserve_exact(n as usize);
                    for _ in 0..n {
                        let handle_index = r.u16()?;
                        let arg_count = r.u16()?;
                        let mut args = Vec::with_capacity(arg_count as usize);
                        for _ in 0..arg_count {
                            args.push(r.u16()?);
                        }
                        bootstrap.push(BootstrapMethod { handle_index, args });
                    }
                }
                Some("Signature") => {
                    let sig_index = r.u16()?;
                    signature = pool.utf8(sig_index).map(str::to_string);
                }
                Some("RuntimeVisibleAnnotations") => {
                    let n = r.u16()?;
                    annotations.reserve_exact(n as usize);
                    for _ in 0..n {
                        if let Some(desc) = Self::parse_annotation(r, pool)? {
                            annotations.push(desc);
                        }
                    }
                }
                _ => r.skip(length)?,
            }
        }
        Ok((bootstrap, signature, annotations))
    }

    /// Parse one `annotation` (JVMS 4.7.16): read the type descriptor, then walk
    /// past its element-value pairs. Returns the type descriptor
    /// (e.g. `Lio/netty/channel/ChannelHandler$Sharable;`).
    fn parse_annotation(r: &mut Reader, pool: &ConstantPool) -> Result<Option<String>, ClassError> {
        let type_index = r.u16()?;
        let desc = pool.utf8(type_index).map(str::to_string);
        Self::skip_element_pairs(r, pool)?;
        Ok(desc)
    }

    /// Walk one `annotation` without materialising its type descriptor.
    ///
    /// Only CLASS-LEVEL annotation types are reported (`ClassFile::annotations`); a nested
    /// annotation's own type is never read by anybody. The `b'@'` arm below used to call
    /// `parse_annotation` and drop its `String` on the floor — an allocation whose only
    /// purpose was to be freed. `@Retention`/`@Target`-style meta-annotations nest one deep
    /// on essentially every annotated class.
    fn skip_annotation(r: &mut Reader, pool: &ConstantPool) -> Result<(), ClassError> {
        r.u16()?; // type_index
        Self::skip_element_pairs(r, pool)
    }

    /// The `{ element_name_index, element_value }*` tail shared by both walks above (DRY).
    fn skip_element_pairs(r: &mut Reader, pool: &ConstantPool) -> Result<(), ClassError> {
        let num_pairs = r.u16()?;
        for _ in 0..num_pairs {
            r.u16()?; // element_name_index
            Self::skip_element_value(r, pool)?;
        }
        Ok(())
    }

    /// Skip one `element_value` (JVMS 4.7.16.1) so the reader stays aligned with
    /// the attribute, without materialising the value.
    fn skip_element_value(r: &mut Reader, pool: &ConstantPool) -> Result<(), ClassError> {
        let tag = r.u8()?;
        match tag {
            b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' | b's' => { r.u16()?; }
            b'e' => { r.u16()?; r.u16()?; } // enum: type_name + const_name
            b'c' => { r.u16()?; }           // class_info_index
            b'@' => { Self::skip_annotation(r, pool)?; } // nested annotation
            b'[' => {
                let n = r.u16()?;
                for _ in 0..n { Self::skip_element_value(r, pool)?; }
            }
            _ => {} // unknown tag — leave as-is (malformed); rare
        }
        Ok(())
    }

    /// Fields and methods share the same `{flags, name, desc, attributes}`
    /// layout (JVMS 4.5 / 4.6), so one routine parses both (DRY).
    ///
    /// The two `to_string()`s below are the ~1.8 M-per-boot duplication of pool text; see
    /// [`MemberInfo`] for why they cannot be removed a piece at a time and what order the
    /// removal has to run in.
    ///
    /// `nat`, when supplied, collects the `(name_index, desc_index)` pair of each member — the
    /// key array described above [`pack_nat`]. It is `Some` only for the METHOD table; fields are
    /// never looked up by pool index, so they never pay for it.
    fn parse_members(
        r: &mut Reader,
        pool: &ConstantPool,
        mut nat: Option<&mut Vec<u32>>,
    ) -> Result<Vec<MemberInfo>, ClassError> {
        let count = r.u16()?;
        let mut members = Vec::with_capacity(count as usize);
        // `reserve_exact`, so the `into_boxed_slice` at the call site sees len == cap and does
        // not reallocate: the whole key array is ONE allocation of exactly 4 bytes per method.
        if let Some(keys) = nat.as_deref_mut() {
            keys.reserve_exact(count as usize);
        }
        for _ in 0..count {
            let access_flags = r.u16()?;
            let name_index = r.u16()?;
            let desc_index = r.u16()?;
            let name_txt = pool.utf8(name_index);
            let desc_txt = pool.utf8(desc_index);
            // The invariant the key array exists to carry: the key resolves, through THIS pool,
            // to exactly the two strings copied into the member below.
            let resolved = name_txt.is_some() && desc_txt.is_some();
            if let Some(keys) = nat.as_deref_mut() {
                if resolved {
                    keys.push(pack_nat(name_index, desc_index));
                } else {
                    // POISON THE WHOLE CLASS. A member whose name or descriptor did not resolve
                    // gets the `"<?>"` placeholder, which is NOT what its index says — so a key
                    // hit would return a member the string lookup would not. Rather than spend a
                    // validity bit per slot on a case only a malformed class file reaches, drop
                    // the array: the length test at the call site then sends every lookup of
                    // this class to the cold arm, which is today's behaviour exactly.
                    keys.clear();
                }
            }
            if !resolved {
                nat = None;
            }
            let name = name_txt.unwrap_or("<?>").to_string();
            let descriptor = desc_txt.unwrap_or("<?>").to_string();
            let code = Self::parse_member_attributes(r, pool)?;
            members.push(MemberInfo { access_flags, name, descriptor, code });
        }
        Ok(members)
    }

    /// Walk a member's `attributes` table, decoding `Code` and skipping the
    /// rest. Other attribute bodies aren't needed until later passes.
    fn parse_member_attributes(
        r: &mut Reader,
        pool: &ConstantPool,
    ) -> Result<Option<Code>, ClassError> {
        let count = r.u16()?;
        let mut code = None;
        for _ in 0..count {
            let name_index = r.u16()?;
            let length = r.u32()? as usize;
            if pool.utf8(name_index) == Some("Code") {
                code = Some(Code::parse(r)?);
            } else {
                r.skip(length)?;
            }
        }
        Ok(code)
    }

    /// Find a method by exact name + descriptor (e.g. `main`,
    /// `([Ljava/lang/String;)V`).
    ///
    /// The index and the probe live in [`crate::members`]; this is the seam. `get_or_init` is
    /// the only atomic left on the path — the fold mode used to be a second one, and now rides
    /// inside the table it built.
    pub fn method(&self, name: &str, descriptor: &str) -> Option<&MemberInfo> {
        self.method_lookup
            .get_or_init(|| MethodIndex::build(&self.methods))
            .find(&self.methods, name, descriptor)
    }

    /// **THE INDEX-KEYED LOOKUP.** Resolve a method from the `(name_index, desc_index)` of a
    /// `CONSTANT_NameAndType` — the shape the caller actually holds — instead of from two
    /// `&str` it decoded out of that same pool a moment earlier.
    ///
    /// EXACTLY EQUIVALENT to, and a drop-in for:
    /// ```ignore
    /// let n = pool.utf8(name_index)?;
    /// let d = pool.utf8(desc_index)?;
    /// cf.method(n, d)
    /// ```
    /// for ANY `pool`, including one belonging to a different class — see the block comment above
    /// [`pack_nat`] for the proof and for the single documented difference (an illegal class file
    /// carrying two methods with the same name and descriptor).
    ///
    /// ⚠**THE POOL IS AN ARGUMENT BECAUSE IT IS PART OF THE KEY.** Index 7 of one class's pool
    /// and index 7 of another's are unrelated bytes; a raw index pair probed against the wrong
    /// class would MATCH and hand back a method with an entirely different name. The guard is
    /// therefore an ADDRESS compare between the pool handed in and this class's own — three
    /// instructions, perfectly predicted at a monomorphic site, and impossible to get wrong from
    /// the outside. A super-class walk fails it on the first ancestor and takes the cold arm.
    ///
    /// THE COLD ARM is the existing string lookup, unchanged, and it runs whenever the fast path
    /// cannot prove its precondition: a foreign pool, a synthesized or malformed class (empty key
    /// array), a class with more than [`NAT_SCAN_MAX`] methods, or a key miss — which is not
    /// proof of absence, because a pool may legally hold two Utf8 entries spelling one text.
    /// Cost of that arm over today: one address compare and one length test.
    ///
    /// KILL SWITCH `RAVA_NO_FAST_CFMETHOD2=1` (and the block switch `RAVA_NO_EMIT_MORE=1`),
    /// consulted once per class at parse time — it empties the key array, so this becomes the
    /// cold arm for every call and the allocation disappears with it.
    #[inline]
    pub fn method_nat(
        &self,
        pool: &ConstantPool,
        name_index: u16,
        desc_index: u16,
    ) -> Option<&MemberInfo> {
        if let Some(i) = self.method_slot_nat(pool, name_index, desc_index) {
            return Some(&self.methods[i]);
        }
        // COLD ARM — byte for byte what every caller does today.
        let name = pool.utf8(name_index)?;
        let descriptor = pool.utf8(desc_index)?;
        self.method(name, descriptor)
    }

    /// The raw probe behind [`method_nat`]: the index into [`Self::methods`], or `None`.
    ///
    /// ⚠`None` MEANS "NOT UNDER THESE INDICES", NOT "ABSENT". Use this only where a `None` can
    /// take a slow path anyway — an inline cache filling its slot, a `CallPlan` deciding whether
    /// it may store a `midx`. Anything that needs a definitive answer must call [`method_nat`],
    /// which supplies the fallback.
    ///
    /// Public because the whole point of the index key is to be usable from the interpreter's
    /// resolution sites; it has no in-crate caller other than [`method_nat`] and the tests.
    #[inline]
    pub fn method_slot_nat(
        &self,
        pool: &ConstantPool,
        name_index: u16,
        desc_index: u16,
    ) -> Option<usize> {
        let keys = &self.method_nat_keys;
        // THREE TESTS, ALL ON DATA ALREADY IN HAND, NO ATOMIC:
        //  * the pool must be the one the keys index into (identity, not equality — see above);
        //  * a short array is the poison/synthesized/kill-switch signal;
        //  * over the cap the hashed string table is the better structure, so hand off.
        if !std::ptr::eq(pool, &self.pool)
            || keys.len() != self.methods.len()
            || keys.len() > NAT_SCAN_MAX
        {
            return None;
        }
        nat_scan(keys, pack_nat(name_index, desc_index))
    }

    /// [`method_nat`] from the `CONSTANT_NameAndType` index a `MethodRef`/`InterfaceMethodRef`
    /// carries, which is the form an `invoke*` site holds after one pool load.
    ///
    /// A non-`NameAndType` entry is `None`, matching `ConstantPool::name_and_type`.
    #[inline]
    pub fn method_of_nat(&self, pool: &ConstantPool, nat_index: u16) -> Option<&MemberInfo> {
        match pool.get(nat_index)? {
            Constant::NameAndType { name_index, desc_index } => {
                self.method_nat(pool, *name_index, *desc_index)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_class_bytes() {
        assert!(matches!(
            ClassFile::parse(&[0, 1, 2, 3]),
            Err(ClassError::BadMagic(_))
        ));
    }

    #[test]
    fn empty_is_eof() {
        assert!(matches!(
            ClassFile::parse(&[]),
            Err(ClassError::UnexpectedEof)
        ));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // THE POOL-INDEX KEY. These parse REAL class bytes rather than using
    // `synthesize`, because the whole mechanism is about indices into a pool and
    // a synthesized class has no pool text behind its members — it exercises only
    // the cold arm (which is also asserted, below).
    // ─────────────────────────────────────────────────────────────────────────

    fn be16(out: &mut Vec<u8>, v: u16) {
        out.extend_from_slice(&v.to_be_bytes());
    }
    fn utf8_entry(out: &mut Vec<u8>, s: &str) {
        out.push(1);
        be16(out, s.len() as u16);
        out.extend_from_slice(s.as_bytes());
    }

    /// A class `X` with `n` methods `m0..m{n-1}`, all `()V`.
    ///
    /// POOL LAYOUT (1-based), chosen so every index the tests need is arithmetic:
    ///   1                Utf8 "X"          2  Class -> 1
    ///   3 + 2i           Utf8 "m{i}"       4 + 2i  Utf8 "()V"   (per method)
    ///   3 + 2n           Utf8 "m0"     — a SECOND slot spelling method 0's name
    ///   4 + 2n           Utf8 "()V"    — and a second spelling its descriptor
    ///   5 + 2n           NameAndType(3, 4) — method 0, the shape an invoke site holds
    /// The duplicate pair is the case that makes a key MISS non-authoritative, and it is
    /// legal: nothing in JVMS 4.4.7 forbids two Utf8 entries with the same bytes.
    fn build_class(n: usize) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
        be16(&mut b, 0); // minor
        be16(&mut b, 52); // major
        be16(&mut b, (6 + 2 * n) as u16); // constant_pool_count = last usable index + 1
        utf8_entry(&mut b, "X"); // 1
        b.push(7); // 2: Class
        be16(&mut b, 1);
        for i in 0..n {
            utf8_entry(&mut b, &format!("m{i}")); // 3 + 2i
            utf8_entry(&mut b, "()V"); // 4 + 2i
        }
        utf8_entry(&mut b, "m0"); // 3 + 2n
        utf8_entry(&mut b, "()V"); // 4 + 2n
        b.push(12); // 5 + 2n: NameAndType
        be16(&mut b, 3);
        be16(&mut b, 4);
        be16(&mut b, 0x0021); // access_flags: public super
        be16(&mut b, 2); // this_class
        be16(&mut b, 0); // super_class (none)
        be16(&mut b, 0); // interfaces_count
        be16(&mut b, 0); // fields_count
        be16(&mut b, n as u16); // methods_count
        for i in 0..n {
            be16(&mut b, 0x0001); // public
            be16(&mut b, (3 + 2 * i) as u16);
            be16(&mut b, (4 + 2 * i) as u16);
            be16(&mut b, 0); // attributes_count
        }
        be16(&mut b, 0); // class attributes_count
        b
    }

    /// The contract in one test: an index hit is the SAME `&MemberInfo` the string lookup
    /// returns, and every way of missing still returns it through the cold arm.
    #[test]
    fn index_key_agrees_with_the_string_lookup() {
        let n = 3usize;
        let cf = ClassFile::parse(&build_class(n)).expect("parses");
        assert_eq!(cf.methods.len(), n);
        for i in 0..n {
            let (ni, di) = ((3 + 2 * i) as u16, (4 + 2 * i) as u16);
            let by_str = cf.method(&format!("m{i}"), "()V").expect("present") as *const MemberInfo;
            let by_nat = cf.method_nat(&cf.pool, ni, di).expect("present") as *const MemberInfo;
            assert_eq!(by_str, by_nat, "index and string lookup must name one member");
            if flags::fast_cfmethod2() {
                assert_eq!(cf.method_slot_nat(&cf.pool, ni, di), Some(i));
            }
        }
        // A DUPLICATE-TEXT pool pair: the raw probe misses (those are not the indices the
        // member was parsed from) and the wrapper still resolves it through the cold arm.
        let (dn, dd) = ((3 + 2 * n) as u16, (4 + 2 * n) as u16);
        assert_eq!(cf.method_slot_nat(&cf.pool, dn, dd), None);
        assert_eq!(
            cf.method_nat(&cf.pool, dn, dd).expect("present") as *const MemberInfo,
            cf.method("m0", "()V").expect("present") as *const MemberInfo
        );
        // From a NameAndType, the form an invoke site actually holds.
        assert_eq!(
            cf.method_of_nat(&cf.pool, (5 + 2 * n) as u16).expect("present") as *const MemberInfo,
            cf.method("m0", "()V").expect("present") as *const MemberInfo
        );
        // Non-NameAndType, and a well-formed pair that names no method: both `None`, exactly
        // as `pool.utf8(..).zip(..).and_then(|..| cf.method(..))` would be.
        assert!(cf.method_of_nat(&cf.pool, 1).is_none());
        assert!(cf.method_nat(&cf.pool, 1, 4).is_none()); // name "X", descriptor "()V"
        assert!(cf.method_nat(&cf.pool, 0, 0).is_none()); // index 0 is Unusable
        assert!(cf.method_nat(&cf.pool, 60000, 60001).is_none()); // off the end
    }

    /// ⚠**THE WORD-IDENTITY GUARD.** Two classes parsed from the same bytes have identical
    /// index pairs and DIFFERENT pools. Probing one class with the other's pool must not take
    /// the fast path — that is precisely the "a profile and a guard naming different words"
    /// failure — and must still return the right method through the cold arm.
    #[test]
    fn a_foreign_pool_never_takes_the_fast_path() {
        let bytes = build_class(3);
        let a = ClassFile::parse(&bytes).expect("parses");
        let b = ClassFile::parse(&bytes).expect("parses");
        assert!(!std::ptr::eq(&a.pool, &b.pool));
        assert_eq!(a.method_slot_nat(&b.pool, 3, 4), None);
        assert_eq!(
            a.method_nat(&b.pool, 3, 4).expect("present") as *const MemberInfo,
            a.method("m0", "()V").expect("present") as *const MemberInfo
        );
    }

    /// Over [`NAT_SCAN_MAX`] the scan hands off to the hashed string table, and the ANSWER is
    /// unchanged — the cap is a routing decision, never a visible one.
    #[test]
    fn a_big_class_hands_off_to_the_string_table() {
        let n = NAT_SCAN_MAX + 6;
        let cf = ClassFile::parse(&build_class(n)).expect("parses");
        for i in [0usize, 1, n / 2, n - 1] {
            let (ni, di) = ((3 + 2 * i) as u16, (4 + 2 * i) as u16);
            assert_eq!(cf.method_slot_nat(&cf.pool, ni, di), None, "over the cap");
            assert_eq!(
                cf.method_nat(&cf.pool, ni, di).expect("present") as *const MemberInfo,
                cf.method(&format!("m{i}"), "()V").expect("present") as *const MemberInfo
            );
        }
    }

    /// A synthesized class has no pool behind its members, so the key array is empty and every
    /// lookup takes the cold arm. Nothing may panic and nothing may be found by index.
    #[test]
    fn a_synthesized_class_has_no_index_key() {
        let cf = ClassFile::synthesize(
            ConstantPool::from_entries(vec![Constant::Unusable]),
            0,
            "X".to_string(),
            None,
            Vec::new(),
            vec![MemberInfo {
                access_flags: 0,
                name: "m0".to_string(),
                descriptor: "()V".to_string(),
                code: None,
            }],
        );
        assert_eq!(cf.method_slot_nat(&cf.pool, 3, 4), None);
        assert!(cf.method_nat(&cf.pool, 3, 4).is_none());
        assert!(cf.method("m0", "()V").is_some(), "the string lookup still works");
    }

    /// The vector scan is `keys.iter().position(..)` — proved here over every length that
    /// exercises the `< 4` form, the whole-chunk form and each of the three overlapping tails,
    /// for a key that is present at every position and for one that is absent.
    #[test]
    fn nat_scan_matches_the_scalar_definition() {
        for n in 0..40usize {
            let keys: Vec<u32> = (0..n as u32).map(|k| k * 7 + 1).collect();
            for probe in 0..(n as u32 * 7 + 12) {
                assert_eq!(
                    nat_scan(&keys, probe),
                    nat_scan_scalar(&keys, probe),
                    "n={n} probe={probe}"
                );
            }
        }
        // Duplicates must report the LOWEST index, as `position` does — including a duplicate
        // that straddles the overlapping tail window.
        for n in 4..24usize {
            let mut keys: Vec<u32> = (0..n as u32).collect();
            keys[n - 1] = 0;
            assert_eq!(nat_scan(&keys, 0), Some(0), "n={n}");
        }
    }
}
