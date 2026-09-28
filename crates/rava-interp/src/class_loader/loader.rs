//! The class loader itself.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::class_loader::*;

/// Hot string-keyed lookup counts (`RAVA_LOOKUP_COUNT`).
pub static SUPER_CHAIN_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static GET_OR_LOAD_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `read_resource` repetition census (`RAVA_RES_STAT`).
pub static RES_LOOKUPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static RES_DISTINCT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static RES_SEEN: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
    std::sync::Mutex::new(None);

impl ClassLoader {
    /// M3 9.2c engine-mutation probe: count of resolved (cached) classes. Growth between two
    /// initCache calls = a class load (shared-table mutation) — a hazard for `*const Vm` shared read.
    pub(crate) fn loaded_count(&self) -> usize {
        self.cache_r().len()
    }
}

/// THE LOADER IS SHARED-READABLE.
///
/// The gil-free lane's ceiling is one global lock held across a worker's whole execution, and it
/// exists to supply the exclusive `&mut Vm` the interpreter's signature demands — not because the
/// work mutates (measured: two touches of frozen shared state across a ring region and 292 million
/// calls). Every one of the seventeen `&mut` leaves the interpreter reaches bottoms out here, in
/// `get_or_load`, so this is the keystone: with the loader's own state behind interior mutability
/// the whole chain above it can take `&Vm` by signature change alone, with no duplicated logic.
///
/// The locks are chosen for the access pattern, not uniformly. `cache` and `missing` are read on
/// every lookup and written only on a miss, so they are `RwLock` — an uncontended read is a few
/// nanoseconds against a `get_or_load` already measured at 1.37% of blocks A+B and absent from the
/// ring's top-15 entirely. `epoch` is a counter and becomes an atomic.
///
/// ⛔`sources` WAS "touched only on a miss (zip inflate), so it is a plain `Mutex`" — and that
/// premise was wrong in the half of the boot that matters. The datapack reload asks the
/// classpath `exists` / `size` / `newInputStream` for every file it reads, and all three land
/// here; none of them mutates anything, and only the last one inflates. So it is an `RwLock`
/// over a `Vec` of sources whose jars each carry their OWN `RwLock` (see `Source`): a scan
/// across N jars now takes N shared reads and one exclusive lock on the jar that answers,
/// instead of one exclusive lock per source held across the inflate.
pub struct ClassLoader {
    sources: std::sync::RwLock<Vec<Source>>,
    /// Classpath entries added at RUNTIME (`add_classpath` — the bundler's extracted
    /// server jar, URLClassLoader adds). Serialized into a checkpoint so a restore
    /// re-adds them before rebuilding frames (their classes aren't on the base path).
    pub added_paths: Vec<String>,
    /// The canonicalised filesystem path of every source that was added BY PATH — the identity
    /// `add_classpath` deduplicates on, so a classpath entry the loader already holds is
    /// recognised instead of re-opened (see [`fast_superchain`]).
    ///
    /// ⚠**A `Vec`, SCANNED LINEARLY, ON PURPOSE.** A full Minecraft classpath is on the order of
    /// forty entries and this is consulted once per `add_classpath` call, i.e. tens of times a
    /// boot against a `Jar::open` apiece. A hash set here would be a table built to answer a
    /// question asked forty times.
    ///
    /// ⚠**`with_jar` CONTRIBUTES NOTHING TO IT**, because a `Jar` does not retain the path it was
    /// opened from. A base-classpath jar re-added at runtime therefore still appends, exactly as
    /// today — the dedup only ever REMOVES work it can prove is redundant, and an unknown path is
    /// not proof.
    source_paths: Vec<PathBuf>,
    /// ⛔**THE KEY STAYS A NAME, AND THE REASON IS THE CALLERS, NOT THE HASH.** Re-keying this on
    /// a dense class id would turn every lookup into an integer probe — but a `ClassId` has to
    /// come from somewhere, and every one of the ~17 `&mut Vm` leaves that bottoms out in
    /// `get_or_load` arrives holding a `&str` off the constant pool. Interning it to get the id is
    /// the same hash this map already does, plus a second table, so the id only pays once the
    /// CALLERS hold ids — a constant-pool-side change, not a loader-side one. The hash itself is
    /// already the word-at-a-time `FxHasher` (`fast_strhash`), eight rounds for a 62-byte name.
    ///
    /// ⛔**AND THE `Arc<LoadedClass>` CLONE ON EVERY HIT STAYS TOO.** ~6.5 M of the boot's 11 M
    /// lookups hit, and each pays an increment and a decrement on one shared refcount line. The
    /// only way to not pay it is to hand back a reference borrowed from the read guard, and the
    /// guard cannot outlive this function while callers hold `Arc<LoadedClass>` in their own
    /// structures (`CallPlan::decl`, the JIT's resolved sites). Removing it is a signature change
    /// across the tree, not an edit here — and row 284 records the sibling experiment, where
    /// trading an allocation for exactly this atomic measured SLOWER twice.
    cache: std::sync::RwLock<crate::FastMap<String, Arc<LoadedClass>>>,
    /// Names proven absent from every source. Negative lookups dominate the
    /// hot path: `instanceof`/`name_is_subtype` probe JDK super names (handled
    /// natively, never in the jar) on every check, and without this each probe
    /// re-ran a zip `by_name` miss + a `format!`/`replace` string allocation.
    /// Cleared whenever a source is added (a name may then resolve).
    missing: std::sync::RwLock<HashSet<Box<str>, std::hash::BuildHasherDefault<crate::FxHasher>>>,
    /// Memoised superclass chains (class → its ancestor names incl. Object).
    /// Walked on every `instanceof` / native-resolution fallback and rebuilt a
    /// fresh `Vec<String>` per call; the chain is stable unless a source is
    /// added, so cache the `Arc<[Box<str>]>` and hand back a refcount bump.
    super_chain_cache: std::sync::RwLock<crate::FastMap<Box<str>, Arc<[Box<str>]>>>,
    /// Memoised TYPE CLOSURES (class → every ancestor AND interface name, transitively).
    /// `super_chain_cache` above answers "what does this extend"; this answers "what is
    /// this a", which is a different set and the one subtype questions actually ask.
    type_closure_cache: std::sync::RwLock<crate::FastMap<Box<str>, Arc<crate::FastSet<Box<str>>>>>,
    /// ⭐**THE DOWNWARD INDEX — the only hierarchy table in this loader that points at CHILDREN.**
    /// `super_chain_cache` and `type_closure_cache` both answer "what is this a"; class-hierarchy
    /// analysis asks the opposite question, "who is this", and answering it by scanning the whole
    /// loaded set is what made the first CHA round cost more compile time than the better code
    /// bought back (+6.8% cold against -4.8% warm). Built in ONE pass over the cache from
    /// `super_class` and `interfaces`, tagged with the epoch it was built at, and rebuilt only
    /// when a query arrives at a newer epoch.
    /// `bool` = "the initial full pass has run"; after that the map is kept CURRENT by
    /// `note_sub_edges` on every publication, so it never needs rebuilding.
    sub_index: std::sync::RwLock<(bool, crate::FastMap<Box<str>, Vec<Box<str>>>)>,
    /// GAP-S06/S02: the HIERARCHY EPOCH — bumped whenever the set of loaded
    /// classes (or the source list) changes. Any analysis whose verdict ranges
    /// over "every currently loaded class" (an exact dispatch closure) is valid
    /// only while this value is unchanged; consumers must re-derive after a bump.
    epoch: std::sync::atomic::AtomicU64,
    /// ⭐**THE SOURCE-LIST EPOCH, WHICH IS NOT THE HIERARCHY EPOCH.** `epoch` above moves on
    /// every class LOAD (~15,000 a boot); the classpath itself changes ~43 times, all of them
    /// back to back while the bundler's `URLClassLoader` registers its extracted libraries.
    /// The path index below is keyed on THIS counter, so a class load does not invalidate it.
    /// Bumped inside the `sources` write guard so an index built under the read guard sees a
    /// consistent (list, epoch) pair.
    sources_epoch: std::sync::atomic::AtomicU64,
    /// The merged, sorted name index of every JAR source — see [`PathIndex`]. `None` until the
    /// first resource question arrives (the boot asks nothing before the classpath is complete),
    /// and rebuilt whenever `sources_epoch` moves past it.
    path_index: std::sync::RwLock<Option<Arc<PathIndex>>>,
}

/// ⭐**EVERY RESOURCE QUESTION SCANNED THE WHOLE CLASSPATH; THE ANSWER IS AN INDEX.**
///
/// A Minecraft boot's classpath is not two entries. It is the bundler jar, the extracted server
/// jar, and every library the bundler's `URLClassLoader` registers — on the order of forty jars,
/// and the datapack reload asks `exists` / `size` / `isDirectory` / `newInputStream` per file
/// while class loading asks for ~15,000 `{name}.class`. Each of those questions walked all forty
/// sources, taking a lock and hashing the path once per source to be told "no" thirty-nine times.
///
/// Worse, the PREFIX questions had the wrong complexity outright. `Jar::entries_under` is a
/// linear scan of that jar's whole name list which CLONES every match, so one
/// `Files.walk`/`Files.find`/`Files.list` cost a pass over every entry name of every jar — on the
/// order of a hundred thousand `starts_with` calls plus a `BTreeSet<String>` rebuilt from
/// scratch — and MC's pack scan asks per directory, per namespace, per registry. That is a
/// directory listing rebuilt per query, which no JIT can amortise because the reload runs once.
///
/// So the names are folded ONCE into a single sorted array and every question becomes a binary
/// search:
///   * an exact name yields the (tiny) run of sources holding it, so a scan skips a jar with an
///     integer compare instead of a lock and a hash, and a name no jar holds skips all of them;
///   * a prefix yields a CONTIGUOUS SLICE — the listing is the answer's own size, not the
///     archive's.
///
/// **SIZE: ~7 MB for a full MC classpath** — one `blob` of ~100,000 concatenated entry names
/// (~4.5 MB, ONE allocation, not 100,000) plus 24 bytes of `PathEnt` each (~2.4 MB). It is a
/// cold table consulted a few times per resource question and makes no claim on L1; the jars it
/// replaces already hold the same names in a `HashSet` and a `HashMap` apiece.
///
/// Entries are NOT deduplicated across jars: which source owns a name is exactly what the
/// order-sensitive callers (`read_bytes`, `read_resource`, `resource_size` all take the FIRST
/// source that answers) need, and duplicate names are a handful.
///
/// ⚠**JARS ONLY.** A `Source::Dir`'s tree is mutable while the server runs, so indexing it would
/// cache a directory listing that the filesystem can invalidate under us. Dir sources are still
/// walked live on every query; only the jars — whose central directory is fixed at `Jar::open` —
/// are folded in.
///
/// ⚠**A REBUILD IS O(WHOLE CLASSPATH), AND THAT IS SAFE ONLY BECAUSE ADDITIONS COME IN BATCHES.**
/// Building costs one `entries_under("")` per jar (the only enumeration `Jar` exposes, and it
/// clones) plus one sort — tens of milliseconds at MC's size — and a classpath addition throws
/// the whole thing away. It survives because the additions are batched by construction: the
/// bundler's `URLClassLoader` constructor registers every extracted library inside ONE native
/// call, and checkpoint restore replays them in one loop, so no class load can interleave and
/// force a rebuild between two adds. If a workload ever DOES interleave them, the fix is not a
/// heuristic: `sources` is append-only, so the index can merge the new source's (separately
/// sorted) names into the existing run in O(N+M) instead of re-deriving everything.
struct PathIndex {
    /// The `sources_epoch` this was built at. A mismatch means a classpath entry was added and
    /// every answer derived from this index may be missing a source.
    epoch: u64,
    /// Every jar entry name, concatenated in one allocation. `PathEnt` slices into it.
    blob: String,
    /// Sorted by (name, source index): a name's owners are adjacent and in classpath order, and
    /// a prefix's matches are one contiguous run.
    ents: Box<[PathEnt]>,
    /// ⭐**TWO FACTS ABOUT THE CLASSPATH THE BUILDER ALREADY SEES, AND EVERY QUERY RE-DERIVED.**
    ///
    /// The index answers for JARS ONLY, so five of the six resource questions end with "…and now
    /// ask every directory source", and two more open with "is there a jar at all". Each of those
    /// was a `sources` read guard plus a walk of the whole `Vec<Source>` — O(sources) per query on
    /// a classpath that is the bundler jar, the extracted server jar and every library the
    /// bundler's `URLClassLoader` registers, i.e. ~40 entries and growing with every
    /// `add_classpath`. The answer cannot change while the index is valid: both are functions of
    /// the same source list, tagged with the same `sources_epoch`, and a source addition throws
    /// the index away. Two bools against a ~6 MB structure.
    has_dirs: bool,
    any_jar: bool,
    /// ⭐**THE GATE WORD THE INDEX WAS BUILT WITH — SO A COMPARATOR NEVER TOUCHES TLS.**
    ///
    /// `name_lt` / `name_eq` / `under`'s `starts` predicate ran `fast_pathkey()` — an ACQUIRE
    /// `OnceLock` load — INSIDE a `partition_point` comparator ~17 probes deep over ~100,000
    /// entries, on the order of a million searches a boot. LLVM may not hoist an acquire load out
    /// of that loop and must keep the probe key and the slice bounds materialised across it, in a
    /// comparator whose real work is a `u64` compare. The switch is a process constant fixed
    /// before `main`, and the index is rebuilt only when the classpath changes, so the whole word
    /// is captured here at build time: a probe now tests a bit of a `u32` on a struct line it has
    /// already loaded. Under `RAVA_NO_FAST_LOADERSHAPE` the word is `LG_LIVE` and every read goes
    /// back through its own `OnceLock` at its own site, historic shape intact.
    gates: u32,
}

/// One jar entry: where its name lives in `PathIndex::blob`, and which classpath source holds it.
///
/// ⭐**`key` EXISTS TO BREAK A DEPENDENT LOAD CHAIN, NOT TO SAVE INSTRUCTIONS.** Every probe of
/// every binary search below used to be: load the `PathEnt` (one cache line), then follow `off`
/// into a ~4.5 MB `blob` (a SECOND, unpredictable miss), then call `memcmp`. The searches are
/// ~17 probes deep over ~100,000 entries and the boot runs on the order of a million of them
/// (~15,000 class loads plus every `exists`/`size`/`isDirectory`/`newInputStream` of the datapack
/// reload), so the blob load is the cost — this is a pointer-chase, not arithmetic, and no
/// vectorisation touches it. `key` puts the discriminating bytes IN the entry, so the common
/// probe resolves from the line already loaded and never reaches the blob at all.
#[derive(Clone, Copy)]
struct PathEnt {
    /// The first 8 bytes of the name, big-endian, right-padded with zero bytes — i.e. the first
    /// 8 bytes of the name's infinite zero-extension, read as a `u64`.
    ///
    /// ⭐**WHY COMPARING THESE IS EXACTLY COMPARING THE NAMES.** For byte strings A and B let A∞
    /// be A followed by infinite zeros. If A∞ ≠ B∞ then lex(A∞,B∞) = lex(A,B): at the first
    /// differing index i, either both names are longer than i (they differ there, and agree
    /// before, so the byte decides both) or the shorter one ended at i — making it a strict
    /// prefix of the other, which lex-orders it Less, and its pad byte 0 is below the other's
    /// (necessarily nonzero) byte, so the extension orders it Less too. Big-endian `u64` order IS
    /// lexicographic byte order, so:
    ///   * `a.key != b.key` ⇒ the first difference of the extensions is inside the first 8 bytes
    ///     ⇒ `a.key.cmp(&b.key)` IS `name(a).cmp(name(b))`. Decide and stop.
    ///   * `a.key == b.key` ⇒ undecided; fall through to the full `str` compare.
    /// The key therefore never DISAGREES with the string order, it only sometimes ABSTAINS — so
    /// every predicate that consults it computes the identical boolean, the sort produces the
    /// identical permutation, and `partition_point`'s monotonicity precondition is untouched.
    /// This holds for empty names and for names containing NUL, neither of which zip forbids.
    key: u64,
    off: usize,
    len: u32,
    src: u32,
}

/// The first 8 bytes of `s`'s zero-extension as a big-endian `u64` — see [`PathEnt::key`].
///
/// The ≥8 arm is ONE unaligned 8-byte load plus a `bswap`; the tail arm is scalar precisely
/// because a wide load at the end of a short name would read past the allocation (`blob` is
/// exactly the concatenated names, with no slack), and a page-boundary read there faults about
/// one time in a hundred rather than every time.
#[inline(always)]
fn prefix_key(s: &str) -> u64 {
    let b = s.as_bytes();
    if b.len() >= 8 {
        // In bounds by the length test, so every check folds away: this is one `mov` + `bswap`.
        u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
    } else {
        let mut k = 0u64;
        let mut i = 0;
        while i < b.len() {
            k |= (b[i] as u64) << (56 - 8 * i);
            i += 1;
        }
        k
    }
}

/// The key of the VIRTUAL name `dir + "/"`, built without allocating it — the sort position
/// `any_jar_dir` searches for. Identical to `prefix_key(&format!("{dir}/"))` by construction.
#[inline(always)]
fn dir_prefix_key(dir: &str) -> u64 {
    let b = dir.as_bytes();
    if b.len() >= 8 {
        return prefix_key(dir);
    }
    let mut k = 0u64;
    let mut i = 0;
    while i < b.len() {
        k |= (b[i] as u64) << (56 - 8 * i);
        i += 1;
    }
    // The '/' that the virtual key appends lands in the next byte slot; the rest stays zero pad.
    k | ((b'/' as u64) << (56 - 8 * b.len()))
}

impl PathIndex {
    #[inline]
    fn name<'a>(&'a self, e: &PathEnt) -> &'a str {
        &self.blob[e.off..e.off + e.len as usize]
    }

    /// `self.name(e) < probe`, deciding from `e.key` alone whenever the keys differ.
    /// `k` must be `prefix_key(probe)` and `pk` [`Self::pathkey`] — BOTH hoisted out of the search
    /// by every caller, because both are loop invariants and one of them used to be an acquire
    /// load the optimiser could not lift (see [`PathIndex::gates`]).
    #[inline(always)]
    fn name_lt(&self, e: &PathEnt, probe: &str, k: u64, pk: bool) -> bool {
        if pk && e.key != k {
            return e.key < k;
        }
        self.name(e) < probe
    }

    /// `self.name(e) == probe`. A key mismatch is a definitive NO (equal names have equal keys),
    /// so the blob is touched only for entries that already agree on their first 8 bytes.
    #[inline(always)]
    fn name_eq(&self, e: &PathEnt, probe: &str, k: u64, pk: bool) -> bool {
        if pk && e.key != k {
            return false;
        }
        self.name(e) == probe
    }

    /// This index's inline-key bit, read off the word it was built with — a plain field load LLVM
    /// is free to hoist out of a comparator, where the `OnceLock` it replaces was not.
    #[inline(always)]
    fn pathkey(&self) -> bool {
        lg_of(self.gates, LG_PATHKEY, fast_pathkey_live)
    }

    /// This index's third-pass bit, read the same way.
    #[inline(always)]
    fn loader3(&self) -> bool {
        lg_of(self.gates, LG_LOADER3, fast_loader3_live)
    }

    fn build(sources: &[Source], epoch: u64) -> Self {
        // Resolved ONCE for the life of the index, and stored on it — see `PathIndex::gates`.
        let gates = loader_gates();
        let pk = lg_of(gates, LG_PATHKEY, fast_pathkey_live);
        let mut cap = 0usize;
        for s in sources {
            if let Source::Jar(j) = s {
                cap += j.read().unwrap_or_else(|e| e.into_inner()).entry_count();
            }
        }
        let mut blob = String::with_capacity(cap * 48);
        let mut ents: Vec<PathEnt> = Vec::with_capacity(cap);
        for (i, s) in sources.iter().enumerate() {
            if let Source::Jar(j) = s {
                // `entries_under("")` is the only name enumeration `Jar` exposes, and it clones.
                // Paid ONCE per classpath change, against the per-query scan it retires.
                for n in j.read().unwrap_or_else(|e| e.into_inner()).entries_under("") {
                    ents.push(PathEnt {
                        key: prefix_key(&n),
                        off: blob.len(),
                        len: n.len() as u32,
                        src: i as u32,
                    });
                    blob.push_str(&n);
                }
            }
        }
        let b = &blob;
        // ⭐**THE SORT IS ONE-SHOT AND STILL WORTH THE KEY.** ~100,000 entries is ~1.7 M
        // comparisons, and jar names share enormous prefixes (`net/minecraft/…` for every
        // classfile), so the old comparator chased TWO random blob offsets and then ran a
        // `memcmp` that had to walk past the shared prefix before it learned anything. The key
        // decides the cross-namespace pairs — `assets/` vs `data/` vs `net/` vs `META-INF/`,
        // which is most of the merge — from the entries themselves. `then_with` (not `then`)
        // keeps the string compare LAZY, so it is evaluated only on a key tie.
        //
        // The resulting permutation is byte-identical to the old one: by `PathEnt::key`'s
        // argument a key comparison either matches the string comparison or is Equal, so
        // `key.cmp().then_with(str).then(src)` and `str.cmp().then(src)` are the same total
        // order. Sorting by the old comparator with the switch off proves it.
        if pk {
            ents.sort_unstable_by(|x, y| {
                x.key
                    .cmp(&y.key)
                    .then_with(|| {
                        let nx = &b[x.off..x.off + x.len as usize];
                        let ny = &b[y.off..y.off + y.len as usize];
                        nx.cmp(ny)
                    })
                    .then(x.src.cmp(&y.src))
            });
        } else {
            ents.sort_unstable_by(|x, y| {
                let nx = &b[x.off..x.off + x.len as usize];
                let ny = &b[y.off..y.off + y.len as usize];
                nx.cmp(ny).then(x.src.cmp(&y.src))
            });
        }
        Self {
            epoch,
            blob,
            ents: ents.into_boxed_slice(),
            has_dirs: sources.iter().any(|s| matches!(s, Source::Dir(_))),
            any_jar: sources.iter().any(|s| matches!(s, Source::Jar(_))),
            gates,
        }
    }

    /// The run of entries whose name is exactly `name`, in classpath order. Empty means no jar
    /// on the path holds it — the negative answer the per-source scan paid forty locks for.
    ///
    /// Both ends are found by binary search. The lower one always was; the upper one walked the
    /// equal run one entry at a time, which is cheap here (a name has one or two owners) and is
    /// the same code shape as [`Self::under`], where it is not cheap at all — see there.
    fn exact(&self, name: &str) -> &[PathEnt] {
        // Hoisted: the probe's key AND the inline-key switch are loop invariants of both searches
        // below. The switch used to be re-read per PROBE, out of an `OnceLock`, with acquire
        // ordering — see [`PathIndex::gates`] for why that is the expensive half.
        let k = prefix_key(name);
        let pk = self.pathkey();
        let lo = self.ents.partition_point(|e| self.name_lt(e, name, k, pk));
        if self.loader3() {
            // Over `ents[lo..]` every name is >= `name`, so "== name" is a true-run followed by a
            // false-run: monotone, and the search costs log(OWNERS), not log(ENTRIES).
            let hi = lo + self.ents[lo..].partition_point(|e| self.name_eq(e, name, k, pk));
            return &self.ents[lo..hi];
        }
        let mut hi = lo;
        while hi < self.ents.len() && self.name_eq(&self.ents[hi], name, k, pk) {
            hi += 1;
        }
        &self.ents[lo..hi]
    }

    /// Does ANY jar hold this exact name — without delimiting the run of the ones that do.
    ///
    /// `has_resource` asked `exact(path).is_empty()`, i.e. it located the end of a run it then
    /// only compared against zero. One `partition_point` and one string compare answer it.
    fn holds(&self, name: &str) -> bool {
        if !self.loader3() {
            return !self.exact(name).is_empty();
        }
        let k = prefix_key(name);
        let pk = self.pathkey();
        let lo = self.ents.partition_point(|e| self.name_lt(e, name, k, pk));
        self.ents.get(lo).is_some_and(|e| self.name_eq(e, name, k, pk))
    }

    /// The contiguous run of entries whose name starts with `prefix`. Sorted, so the caller gets
    /// the listing in the same order the old `BTreeSet` produced.
    ///
    /// ⭐**FINDING THE END OF THE RUN COST THE RUN.** The forward walk below is O(k) in the
    /// number of entries under `prefix` — and `data/` in MC's server jar is tens of thousands of
    /// them. `list_resources` consumes all k anyway, so it never noticed; `list_resource_children`
    /// does not, and its whole reason for existing is that "one level should not cost the whole
    /// subtree". It called this first, so it paid for the subtree before its own binary searches
    /// ever ran — once per directory node, per namespace, per registry, all through the pack
    /// scan. The upper bound is a binary search like the lower one, so the run is now DELIMITED
    /// in O(log k) and only a caller that reads it pays for its size.
    fn under(&self, prefix: &str) -> &[PathEnt] {
        let k = prefix_key(prefix);
        // The key bytes the prefix actually constrains. A name that starts with `prefix` must
        // agree with it on those bytes (it is at least that long), so a mismatch is a definitive
        // NO and the blob is never touched — which is the whole point on a `data/`-sized run.
        // An empty prefix constrains nothing (mask 0, everything falls through to `starts_with`,
        // which is unconditionally true) — and it also keeps the shift below out of the
        // `<< 64` undefined case.
        let mask = match prefix.len() {
            0 => 0u64,
            n if n >= 8 => u64::MAX,
            n => !0u64 << (64 - 8 * n),
        };
        let masked = k & mask;
        // ⭐**ONE RESOLUTION FOR THE WHOLE QUERY, NOT ONE PER PROBE AND ONE PER RUN STEP.** The
        // fallback walk below steps over a `data/`-sized run entry by entry, so with the switch
        // off this predicate was tens of thousands of acquire loads for ONE listing.
        let pk = self.pathkey();
        let starts = |e: &PathEnt| {
            if pk && (e.key & mask) != masked {
                return false;
            }
            self.name(e).starts_with(prefix)
        };
        let lo = self.ents.partition_point(|e| self.name_lt(e, prefix, k, pk));
        if self.loader3() {
            // Over `ents[lo..]` every name is >= `prefix`, and a name that does not start with
            // `prefix` sorts after every name that does — so `starts_with` is monotone here.
            let hi = lo + self.ents[lo..].partition_point(|e| starts(e));
            return &self.ents[lo..hi];
        }
        let mut hi = lo;
        while hi < self.ents.len() && starts(&self.ents[hi]) {
            hi += 1;
        }
        &self.ents[lo..hi]
    }

    /// `Jar::is_dir` for the WHOLE classpath, decided from the index.
    ///
    /// A jar answers "directory" for `p` iff its `dirs` set holds `p` and its entry set does not
    /// — and `dirs` is exactly "every parent of every entry", so `dirs` holds `p` iff that jar
    /// has some entry under `p/`. Both halves are index facts, so the whole forty-jar scan
    /// (a lock and two hashes each) collapses. The per-source structure is kept rather than
    /// merged because the two halves must be evaluated on the SAME jar: one jar holding `p` as a
    /// file does not stop another that holds `p/q` from calling it a directory.
    fn any_jar_dir(&self, trimmed: &str) -> bool {
        // The virtual key is `trimmed + "/"`, built without allocating it: names starting with
        // `trimmed` are contiguous, and within them the byte at `trimmed.len()` is sorted too.
        //
        // `cmp_dir_key(n, trimmed)` IS `n.cmp(trimmed + "/")`, so `PathEnt::key`'s argument
        // applies verbatim against the virtual name's own key: a differing key decides the
        // ordering outright, an equal one falls through to the exact comparator. The blob load
        // and the two-branch comparator are skipped on every probe that differs early — which,
        // for a `data/<namespace>/<registry>` walk, is nearly all of them.
        let dk = dir_prefix_key(trimmed);
        let pk = self.pathkey();
        let lo = self.ents.partition_point(|e| {
            if pk && e.key != dk {
                return e.key < dk;
            }
            Self::cmp_dir_key(self.name(e), trimmed).is_lt()
        });
        // Resolved ONCE outside the loop: the run can be thousands of entries, and the set of
        // jars holding `trimmed` as a file is the same for every one of them.
        let files = self.exact(trimmed);
        for e in &self.ents[lo..] {
            let n = self.name(e);
            if !(n.len() > trimmed.len()
                && n.as_bytes()[trimmed.len()] == b'/'
                && n.starts_with(trimmed))
            {
                break;
            }
            // This jar has a child of `trimmed`; it calls `trimmed` a directory unless it also
            // holds it as an entry of its own.
            if !files.iter().any(|f| f.src == e.src) {
                return true;
            }
        }
        false
    }

    /// Order of `n` against the virtual key `dir + "/"`.
    fn cmp_dir_key(n: &str, dir: &str) -> std::cmp::Ordering {
        let m = n.len().min(dir.len());
        match n.as_bytes()[..m].cmp(&dir.as_bytes()[..m]) {
            std::cmp::Ordering::Equal => {}
            o => return o,
        }
        if n.len() < dir.len() {
            return std::cmp::Ordering::Less; // `n` is a proper prefix of `dir`
        }
        match n.as_bytes().get(dir.len()) {
            None => std::cmp::Ordering::Less, // n == dir, and dir < dir + "/"
            Some(&b) => b.cmp(&b'/').then(n.len().cmp(&(dir.len() + 1))),
        }
    }
}

// ═══════ THE LOADER GATE WORD — ONE PLAIN LOAD IN FRONT OF NINE PROCESS CONSTANTS ═══════
// (`RAVA_NO_FAST_LOADERSHAPE=1` restores the historic shape; so does `RAVA_NO_EMIT_MORE`.)
//
// **WHAT THE SHAPE WAS.** Every switch in this file is a process constant behind its own
// `OnceLock<bool>`, and each read is an ACQUIRE load. On x86 the `mov` is not the cost: LLVM may
// not hoist an acquire load out of the caller's loop, may not CSE it against the identical load
// three lines down, and has to keep the caller's live values materialised across it. The loader
// then read them one per DECISION rather than one per call:
//
//   * `PathIndex::name_lt` / `name_eq` / `under`'s `starts` closure each read `fast_pathkey()`
//     **inside a `partition_point` comparator** — a binary search ~17 probes deep over ~100,000
//     entries, run on the order of a million times a boot (~15,000 class loads plus every
//     `exists` / `size` / `isDirectory` / `newInputStream` of the datapack reload). That is tens
//     of millions of acquire loads whose answer was fixed before `main` ran, and it sat in the
//     one loop in this file where a barrier costs the most: the comparator is three instructions
//     of real work, so a load the optimiser must not move dominates it.
//   * one `read_bytes` read `fast_jario()` (through `path_index`), `fast_loader3()` twice (once
//     inside `exact`, once for the empty-owners verdict) and `fast_classload()` — four DIFFERENT
//     `OnceLock`s per class load, on top of the per-probe ones. `read_resource` added
//     `RAVA_RES_STAT` to that, and `resource_is_dir` / `has_resource` / `list_resources` each
//     took two more for a question the index had already decided.
//   * `get_or_load` (4.9 M calls a boot) and `super_chain` open with an `OnceLock` that only
//     decides whether a DIAGNOSTIC counter ticks, and `type_closure` with one more.
//
// **WHAT THE SHAPE IS.** All of it is resolved once per thread into a const-initialised,
// destructor-free `thread_local` `Cell<u32>` — the shape rule 1 requires for a TLS slot that
// lowers to a single slot load with no lazy-init check and no `LocalKey::with` call — and read
// back as a PLAIN load that LLVM is free to hoist out of `partition_point` and to CSE across
// every gate in a function. The per-probe population goes further still: [`PathIndex`] stores the
// word it was built with, so a comparator tests a bit of a struct field it has already loaded and
// never touches thread-local storage at all.
//
// ⚠**NO GATE MOVED, AND NOTHING IS RESOLVED THAT WAS NOT.** Every bit is read at the same site,
// guarding the same branch, in the same order; only the LOAD changed. Each bit's initialiser is
// pure (an `env::var_os` presence test, or `diag::selected`, which formats a name and scans an
// already-cached selector) — none of them arms a thread, opens a file or prints, so resolving
// them at this thread's first loader call rather than at its first use of that particular switch
// has no observable effect. Under the kill switch the word carries `LG_LIVE` and NOTHING else, so
// every accessor reaches its own `OnceLock` from its own site exactly as it historically did.

/// Bit 0 — this thread has resolved. A word of 0 therefore means "not yet", which is what makes
/// the fast read a single compare against zero.
pub(crate) const LG_RESOLVED: u32 = 1 << 0;
/// Bit 1 — the kill switch is engaged: distrust every other bit and call the `OnceLock`s from the
/// sites that used to call them.
pub(crate) const LG_LIVE: u32 = 1 << 1;
/// Bit 2 — [`fast_jario`] (`RAVA_NO_FAST_JARIO`).
pub(crate) const LG_JARIO: u32 = 1 << 2;
/// Bit 3 — [`fast_loader3`] (`RAVA_NO_FAST_LOADER3`).
pub(crate) const LG_LOADER3: u32 = 1 << 3;
/// Bit 4 — [`fast_pathkey`] (`RAVA_NO_FAST_PATHKEY`). The per-probe one.
pub(crate) const LG_PATHKEY: u32 = 1 << 4;
/// Bit 5 — [`fast_superchain`] (`RAVA_NO_FAST_SUPERCHAIN`).
pub(crate) const LG_SUPERCHAIN: u32 = 1 << 5;
/// Bit 6 — [`crate::class_loader::fast_classload`] (`RAVA_NO_FAST_CLASSLOAD`).
pub(crate) const LG_CLASSLOAD: u32 = 1 << 6;
/// Bit 7 — the `type_closure` memo (`RAVA_NO_TYPE_CLOSURE`).
pub(crate) const LG_TYPE_CLOSURE: u32 = 1 << 7;
/// Bit 8 — the `get_or_load` call counter (`RAVA_LOOKUP_COUNT` / `RAVA_DIAG=loader`).
pub(crate) const LG_COUNT_LOAD: u32 = 1 << 8;
/// Bit 9 — the `super_chain` call counter (same two triggers, its own diag name).
pub(crate) const LG_COUNT_SUPER: u32 = 1 << 9;
/// Bit 10 — the `read_resource` repetition census (`RAVA_RES_STAT`).
pub(crate) const LG_RES_STAT: u32 = 1 << 10;

thread_local! {
    /// This thread's resolved loader gates. `Cell<u32>` with a `const` initialiser, deliberately:
    /// no `RefCell` borrow flag, no `Drop`, no lazy-init check, so an access is one TLS slot load.
    static LOADER_GATES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// ⭐**THE DRY-RUN ACTIVE FLAG — THE HOT READ.** `get_or_load` consults dry-run state on
    /// every one of its ~4.5 M calls a boot, so the enabled/disabled answer is a `Cell<bool>`
    /// read (one bare `#[thread_local]` load, no `LocalKey::with`, no borrow flag — the exact
    /// shape `GILFREE` uses) kept in lockstep with the `RefCell` below by
    /// [`ClassLoader::with_dry_run_loads`]'s guard. The `RefCell` holds the actual stage.
    static DRY_RUN_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The active dry-run loading scope, if any. `RefCell` (not `Cell`) because the value is a
    /// heap `Option` whose contents must be MUTATED in place through the shared `LocalKey`.
    static DRY_RUN_LOADS: std::cell::RefCell<Option<DryRunScope>> = const { std::cell::RefCell::new(None) };
}

/// One active dry-run scope: parsed classes staged against the loaders that parsed them, keyed
/// by loader identity (the `&ClassLoader`'s address, stable for the loader's lifetime and always
/// alive here — the closure owns the loader through the VM for the whole scope). Keying stages
/// per loader is what keeps two loaders in one scope from seeing each other's parses, matching
/// the per-loader identity the real cache has. Nothing here is `Send`: the whole scope lives on
/// the thread that opened it.
struct DryRunScope {
    stages: crate::FastMap<usize, crate::FastMap<String, Arc<LoadedClass>>>,
}

impl DryRunScope {
    fn stage_for(&mut self, id: usize) -> &mut crate::FastMap<String, Arc<LoadedClass>> {
        self.stages.entry(id).or_default()
    }
    fn stage_of(&self, id: usize) -> Option<&crate::FastMap<String, Arc<LoadedClass>>> {
        self.stages.get(&id)
    }
}

/// Thread-local dry-run state: `true` while a [`ClassLoader::with_dry_run_loads`] scope is open.
/// ONE bare TLS load on the hot path (`get_or_load`, ~4.5 M calls a boot) — a `Cell<bool>` read,
/// no `LocalKey::with`, no borrow flag; the stage itself lives behind `DRY_RUN_LOADS`.
#[inline(always)]
fn dry_run_active() -> bool {
    DRY_RUN_ACTIVE.get()
}

/// The word. One plain per-thread load; never 0 on return.
#[inline(always)]
pub(crate) fn loader_gates() -> u32 {
    let g = LOADER_GATES.get();
    if g != 0 { g } else { loader_gates_cold() }
}

/// First touch on this thread: read the process constants through their `OnceLock`s — the ONLY
/// place that still happens under the default — and cache the answers. Once per thread, so the
/// environment lookups behind them stay exactly as many as they ever were.
#[cold]
#[inline(never)]
fn loader_gates_cold() -> u32 {
    static LIVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let live = *LIVE.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_LOADERSHAPE").is_some() || crate::flags::emit_more_disabled()
    });
    let mut g = LG_RESOLVED;
    if live {
        // Resolve NOTHING else: with the switch engaged each accessor must reach its own
        // `OnceLock` from its own site, in its own order, exactly as before.
        g |= LG_LIVE;
        LOADER_GATES.set(g);
        return g;
    }
    if fast_jario_live() { g |= LG_JARIO; }
    if fast_loader3_live() { g |= LG_LOADER3; }
    if fast_pathkey_live() { g |= LG_PATHKEY; }
    if fast_superchain_live() { g |= LG_SUPERCHAIN; }
    if crate::class_loader::fast_classload_live() { g |= LG_CLASSLOAD; }
    if type_closure_live() { g |= LG_TYPE_CLOSURE; }
    if count_load_live() { g |= LG_COUNT_LOAD; }
    if count_super_live() { g |= LG_COUNT_SUPER; }
    if res_stat_live() { g |= LG_RES_STAT; }
    LOADER_GATES.set(g);
    g
}

/// Read one bit of an ALREADY-LOADED word — the discipline this whole block exists for: a callee
/// that needs three gates takes the caller's word as an argument instead of taking three acquire
/// loads. `live` is the historic `OnceLock` reader, called only under the kill switch.
#[inline(always)]
pub(crate) fn lg_of(g: u32, bit: u32, live: fn() -> bool) -> bool {
    if g & LG_LIVE != 0 { live() } else { g & bit != 0 }
}

/// Read one bit, loading the word first. Use [`lg_of`] wherever one operation reads two or more
/// bits, or reads one bit inside a loop, so the load is shared.
#[inline(always)]
fn lg(bit: u32, live: fn() -> bool) -> bool {
    lg_of(loader_gates(), bit, live)
}

/// ⭐**THE INDEXED RESOURCE PATH** — default ON, `RAVA_NO_FAST_JARIO=1` disables, and so does
/// `RAVA_NO_EMIT_MORE` so the whole round bisects in one run.
///
/// Off, every resource question falls back to the per-source scan it replaced (which keeps its
/// own `RAVA_NO_FAST_CLASSLOAD` behaviour), so the saving is priced rather than assumed.
/// ⭐**READ AS [`LG_JARIO`], NOT BY CALLING THIS.** Every site that wants it (`path_index`, and
/// through it every resource question) already holds the gate word; this is the historic
/// `OnceLock` reader, called once per thread by [`loader_gates_cold`] and per site only under
/// `RAVA_NO_FAST_LOADERSHAPE`.
fn fast_jario_live() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_JARIO").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// ⭐**THE THIRD LOADER PASS** — default ON, `RAVA_NO_FAST_LOADER3=1` disables, and so does
/// `RAVA_NO_EMIT_MORE` so the whole round bisects in one run.
///
/// Everything it gates is the same shape: a bound that was already known, re-derived by walking.
///   * `PathIndex::exact` / `under` find the END of their run by binary search instead of walking
///     it — the difference between O(log k) and O(k) for a `data/` subtree of tens of thousands,
///     which is what `list_resource_children` was paying before its own searches began;
///   * `PathIndex::holds` answers `has_resource` without delimiting a run at all;
///   * `has_dirs` / `any_jar` are read off the index instead of re-walking the `Vec<Source>` under
///     its read guard — O(classpath sources) per resource question becomes O(1), and a name no jar
///     holds on a jars-only classpath now answers without taking the `sources` lock at all;
///   * `list_resource_children` steps over a child's block with a predicate that is actually
///     MONOTONE (`.` sorts below `/`, so the old one was true-false-true over the very
///     interleaving the tests document, and `partition_point` is only defined on a partitioned
///     slice);
///   * `super_chain` stops on a `super` it has already seen — a cyclic hierarchy spun forever,
///     because the walk deduplicated the name it pushed but still followed it.
///
/// Off, every one of them falls back to the exact walk it replaced, so the saving is priced.
/// ⭐**READ AS [`LG_LOADER3`], NOT BY CALLING THIS** — see [`fast_jario_live`]. Twelve sites
/// wanted this bit, three of them inside loops (the `exact`/`under` searches, the child walk in
/// `list_resource_children`, the ancestor walk in `super_chain`).
fn fast_loader3_live() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_LOADER3").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// ⭐**THE INLINE PATH KEY** — default ON, `RAVA_NO_FAST_PATHKEY=1` disables, and so does
/// `RAVA_NO_EMIT_MORE` so the whole round bisects in one run.
///
/// Gates every use of [`PathEnt::key`]: the build sort, `exact`, `holds`, `under` and
/// `any_jar_dir`. Off, each of them runs the exact string comparison it always ran, against the
/// exact same array in the exact same order — the key is still computed and stored (8 bytes an
/// entry, ~800 KB) but never read, so the two arms differ only in whether a probe is allowed to
/// answer from the entry instead of the blob.
///
/// What it costs if removed: every binary-search probe in the classpath index goes back to two
/// dependent cache misses (the entry, then a random offset into a ~4.5 MB name blob) plus a
/// `memcmp` that must walk the shared `net/minecraft/…` prefix before it learns anything, on the
/// order of a million probes a boot.
/// ⭐**READ AS [`PathIndex::pathkey`], NOT BY CALLING THIS.** ⚠Never call it from a comparator:
/// that acquire load in a `partition_point` loop is exactly what this round removed. The searches
/// read the bit off [`PathIndex::gates`], a plain field of a struct they already hold.
fn fast_pathkey_live() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_PATHKEY").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// ⭐**THE CLASSPATH RE-ADD** — default ON, `RAVA_NO_FAST_SUPERCHAIN=1` disables, and so does
/// `RAVA_NO_EMIT_MORE` so the whole round bisects in one run.
///
/// Gates one thing: [`ClassLoader::add_classpath`] recognising a path THAT IS ALREADY A SOURCE
/// and doing nothing, instead of appending a second copy of it.
///
/// ⭐**THE COST OF THE SECOND COPY IS NOT THE SECOND COPY.** `add_classpath` is `URLClassLoader`'s
/// constructor arm — the guest hands it one URL per library, and nothing in the JDK contract says
/// a program builds only one `URLClassLoader`, or that two of them name disjoint jars. Each
/// re-add of a name already on the list pays, in order: a `Jar::open`, which parses that
/// archive's whole central directory (tens of thousands of entries for a server jar) and builds
/// three hash tables from it; a `sources_epoch` bump, which discards the merged [`PathIndex`] —
/// ~7 MB and a sort of ~100,000 names, rebuilt on the next resource question; and
/// `invalidate_for_new_source`, which CLEARS the proven-absent set, the super-chain memo and the
/// type-closure memo. That last one is why this sits under the super-chain switch: every
/// memoised ancestry the boot has derived so far is thrown away, and each one is re-walked from
/// the class file on its next ask, `get_or_load` per level.
///
/// ⭐**AND A DUPLICATE CANNOT CHANGE AN ANSWER, WHICH IS WHY SKIPPING IT IS SOUND.** Every
/// order-sensitive consumer — `read_bytes`, `read_resource`, `resource_size`, `has_resource`,
/// the index's `exact` run — takes the FIRST source that answers. A second copy of a source
/// already on the list is appended BEHIND the first, holds exactly the names the first holds
/// (same file), and is therefore never the source that answers anything. Including when the file
/// changed on disk between the two adds: a `Jar` snapshots its central directory at open, so the
/// first copy already shadows the new content today, and skipping preserves that exactly.
///
/// Off, every add appends unconditionally, exactly as it always did, so the saving is priced.
pub(crate) fn fast_superchain() -> bool {
    lg(LG_SUPERCHAIN, fast_superchain_live)
}
/// The historic reader — see [`fast_jario_live`].
fn fast_superchain_live() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_SUPERCHAIN").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// The historic readers for the three gates that had no accessor of their own — each was an
/// `OnceLock` written out at its single call site, on a path taken millions of times a boot.
fn type_closure_live() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_TYPE_CLOSURE").is_none())
}
fn count_load_live() -> bool {
    static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ARMED.get_or_init(|| {
        crate::env_flag!("RAVA_LOOKUP_COUNT") || crate::diag::selected("loader", "get_or_load_calls")
    })
}
fn count_super_live() -> bool {
    static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ARMED.get_or_init(|| {
        crate::env_flag!("RAVA_LOOKUP_COUNT") || crate::diag::selected("loader", "super_chain_calls")
    })
}
fn res_stat_live() -> bool {
    crate::env_flag!("RAVA_RES_STAT")
}

impl ClassLoader {
    /// ⭐**THE DRY-RUN LOADING SCOPE — parse real classfiles WITHOUT touching loader state.**
    ///
    /// The proof census (`vm.offload_reason_devirt` and friends) must ask "what WOULD this
    /// class's ancestry and hierarchy look like if these classes were loaded?" without the
    /// answer's SIDE EFFECTS: a normal [`Self::get_or_load`] publishes to `cache`, records
    /// `missing`, fills `super_chain_cache` / `type_closure_cache` / `sub_index`, bumps `epoch`
    /// and may force a [`PathIndex`] build through `read_bytes`. Every one of those mutates
    /// shared state whose absence of change the census itself must assert, and several of them
    /// change later abort/routing behaviour (a gil-free worker's future MISS would become a
    /// HIT).
    ///
    /// Inside `f`:
    ///   * classes ALREADY in this loader's real cache stay readable through every ordinary
    ///     helper (the dry path consults the real cache first, read-only);
    ///   * a MISS is served from this loader's STAGE — parsed from the classpath sources with
    ///     the same order and the same classfile semantics, held only in the thread-local
    ///     staging map, repeated lookups hit it, and it DISAPPEARS when the scope closes;
    ///   * a name no source holds is a NotFound that records NOTHING (no `missing` insert);
    ///   * `super_chain` / `type_closure` / `concrete_subtypes` / `loaded_class_names` SEE the
    ///     staged classes, but their shared MEMOS are never written during the scope (reads of
    ///     pre-existing memos are fine — those entries are facts of already-published classes);
    ///   * `epoch`, `loaded_count`, `path_index`, `sources` are untouched by definition: nothing
    ///     here calls any of their writers.
    ///
    /// ⛔**A GIL-FREE WORKER FAILS CLOSED, AS EVERYWHERE ELSE.** A worker inside a dry scope
    /// still cannot parse-and-stage (the parse itself is the shared-read hazard the worker
    /// contract refuses), so [`Self::get_or_load`] keeps its `WorkerRefused` arm UNCHANGED
    /// under the scope: `with_dry_run_loads` on a worker thread stages nothing and every miss
    /// aborts the region exactly as before. Only the serial thread may dry-load.
    ///
    /// The scope is thread-local and NESTED: `with_dry_run_loads` inside
    /// `with_dry_run_loads` sees the outer scope's staged classes and its own, and the outer
    /// scope's stages survive the inner's drop. The staged classes are dropped when the scope
    /// ends — nothing publishes them, and a direct read-only lookup afterwards is the test of
    /// that.
    pub fn with_dry_run_loads<T>(f: impl FnOnce() -> T) -> T {
        if dry_run_active() {
            // Nested scope: the outer one is already staging; the inner one needs no new stage
            // of its own — both query the same thread-local map, and a second (loader, name)
            // entry for the same closure would be an identical answer.
            return f();
        }
        // ⛔THE BORROW MUST NOT SPAN `f`. Every loader query inside `f` (`get_or_load`'s dry
        // arm, the hierarchy helpers) re-enters this `RefCell`, so a `with_borrow_mut` held
        // across `f` would panic on its first nested call. Set and clear in SHORT borrows, and
        // clear through a guard so a panic inside `f` cannot leave the scope open on the thread.
        // The two TLS slots are always moved in lockstep: the flag first (so the scope is live
        // the moment the stage is visible), the stage last (so the flag is never true over a
        // dropped stage).
        DRY_RUN_ACTIVE.set(true);
        DRY_RUN_LOADS.with_borrow_mut(|s| *s = Some(DryRunScope { stages: Default::default() }));
        struct ClearGuard;
        impl Drop for ClearGuard {
            fn drop(&mut self) {
                DRY_RUN_ACTIVE.set(false);
                DRY_RUN_LOADS.with_borrow_mut(|s| *s = None);
            }
        }
        let _guard = ClearGuard;
        f()
    }

    /// Whether a dry-run loading scope is open on this thread. The census consumer's gate: a
    /// proof-cache keyed while this is true must NOT be trusted after the scope ends, because
    /// the closures it derived saw staged (never-published) classes.
    pub fn dry_run_loads_active() -> bool {
        dry_run_active()
    }

    /// How many classes THIS loader has staged in the active scope (0 outside one). Read-only:
    /// the census captures it before/after an analysis to distinguish a proof that needed dry
    /// staging from one that closed over only already-published classes.
    pub fn dry_run_staged_count(&self) -> usize {
        if !dry_run_active() {
            return 0;
        }
        DRY_RUN_LOADS.with_borrow(|s| {
            s.as_ref()
                .and_then(|sc| sc.stage_of(self as *const ClassLoader as usize))
                .map(|m| m.len())
                .unwrap_or(0)
        })
    }
    /// Sorted names successfully parsed into the active dry-run stage for this loader.
    /// Empty outside a dry-run scope; this never publishes or mutates loader state.
    pub(crate) fn dry_run_staged_names(&self) -> Vec<String> {
        if !dry_run_active() {
            return Vec::new();
        }
        let mut names = DRY_RUN_LOADS.with_borrow(|s| {
            s.as_ref()
                .and_then(|scope| scope.stage_of(self as *const ClassLoader as usize))
                .map(|classes| classes.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        });
        names.sort_unstable();
        names
    }

    pub fn new() -> Self {
        Self {
            sources: std::sync::RwLock::new(Vec::new()),
            added_paths: Vec::new(),
            source_paths: Vec::new(),
            cache: std::sync::RwLock::new(crate::FastMap::default()),
            missing: std::sync::RwLock::new(HashSet::default()),
            super_chain_cache: std::sync::RwLock::new(crate::FastMap::default()),
            type_closure_cache: std::sync::RwLock::new(crate::FastMap::default()),
            sub_index: std::sync::RwLock::new((false, crate::FastMap::default())),
            epoch: std::sync::atomic::AtomicU64::new(0),
            sources_epoch: std::sync::atomic::AtomicU64::new(0),
            path_index: std::sync::RwLock::new(None),
        }
    }

    /// GAP-S06/S02: the current hierarchy epoch (see the field doc). A dispatch
    /// closure derived at epoch `e` is stale once `epoch() != e`.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(std::sync::atomic::Ordering::Acquire)
    }

    /// ⭐**CLASS HIERARCHY ANALYSIS: every concrete loaded subtype of `owner`, cheaply.**
    ///
    /// Walks the downward index instead of the loaded set, so the cost is the SUBTREE, not the
    /// world. Returns `None` when the answer is not provable here: more than `cap` subtypes. The
    /// caller only ever asks "is it exactly one", so the cap turns a pathological root like
    /// `java/lang/Object` into an immediate refusal instead of a walk of everything.
    ///
    /// Interfaces ARE valid owners here: the index records `implements` edges alongside `extends`,
    /// which is the half `virtual_dispatch_closure` cannot do — it refuses an interface owner
    /// outright because a lambda implementor is not enumerable from the loader. A lambda still
    /// makes the answer incomplete, which is exactly why the caller's use of this must stay a
    /// GUARDED speculation the emitted diamond re-proves, never an unguarded bind.
    pub(crate) fn concrete_subtypes(&self, owner: &str, cap: usize) -> Option<Vec<Arc<LoadedClass>>> {
        self.ensure_sub_index();
        let ix = self.sub_index.read().unwrap_or_else(|e| e.into_inner());
        let cache = self.cache_r();
        // ⭐DRY-RUN: the downward edge walk consults the STAGE as well as the cache, so a
        // hierarchy question answers over published + staged without any write (the index itself
        // is not built — see `ensure_sub_index` — and `ensure` stays read-only there).
        let stage: Option<crate::FastMap<String, Arc<LoadedClass>>> = if dry_run_active() {
            let id = self as *const ClassLoader as usize;
            DRY_RUN_LOADS.with_borrow(|s| {
                s.as_ref().and_then(|sc| sc.stage_of(id)).map(|m| {
                    m.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect()
                })
            })
        } else {
            None
        };
        let mut out: Vec<Arc<LoadedClass>> = Vec::new();
        let mut stack: Vec<Box<str>> = vec![Box::from(owner)];
        let mut seen: crate::FastSet<Box<str>> = Default::default();
        // ⛔During a dry-run scope the index is NEVER built (see `ensure_sub_index`), so the
        // children of every popped node come from a read-only scan of the REAL cache plus the
        // stage instead. This is O(loaded+staged) per node — fine for the census, which opens a
        // scope for one analysis — and it writes nothing.
        let dry = dry_run_active();
        while let Some(n) = stack.pop() {
            if !seen.insert(n.clone()) {
                continue;
            }
            if let Some(c) = cache.get(&*n).or_else(|| {
                stage.as_ref().and_then(|s| s.get(&*n))
            }) {
                // ACC_INTERFACE 0x0200 | ACC_ABSTRACT 0x0400 — neither can ever be a receiver.
                if c.file.access_flags & 0x0600 == 0 {
                    if out.len() == cap {
                        return None;
                    }
                    out.push(Arc::clone(c));
                }
            }
            if let Some(kids) = ix.1.get(&*n) {
                stack.extend(kids.iter().cloned());
            }
            if dry {
                // A staged class's edges are not in the (not-built, not-published) index, and
                // neither are the REAL cache's; enumerate both directly.
                // This local fallback is O(visited nodes × loaded classes), intentionally confined
                // to the opt-in diagnostic: publishing the shared reverse index would contaminate
                // the dry run. Do not move this scan onto a per-class or ordinary boot path.
                let stage_iter = stage.as_ref().into_iter().flat_map(|s| s.iter());
                for (k, c) in cache.iter().chain(stage_iter) {
                    let is_child = c.file.super_class.as_deref() == Some(n.as_ref())
                        || c.file.interfaces.iter().any(|i| i == n.as_ref());
                    if is_child && seen.insert(Box::from(k.as_str())) {
                        stack.push(Box::from(k.as_str()));
                    }
                }
            }
        }
        Some(out)
    }

    /// ⛔**REBUILDING THIS PER EPOCH COST THE BOOT 17.8% CPU, AND THAT IS WHY IT IS INCREMENTAL.**
    /// The first version stamped the index with `epoch()` and rebuilt whenever a query arrived at
    /// a newer one. On the warm reload that is nearly free — the loaded set is stable and the
    /// index is built once. On a BOOT it is pathological: classes load in their thousands, every
    /// publication bumps the epoch, so a query per compiled method rebuilt the whole map. Measured
    /// interleaved, 3 rounds: wall 37.3/38.4/32.0 against 31.5/35.9/32.2, CPU median +17.78%.
    ///
    /// A downward index does not need epoch invalidation at all — a class publication only ever
    /// ADDS edges, and `note_sub_edges` adds them at the two publication sites. So the full pass
    /// runs once, on first use, over whatever was already loaded.
    fn ensure_sub_index(&self) {
        if self.sub_index.read().unwrap_or_else(|e| e.into_inner()).0 {
            return;
        }
        // ⛔A dry-run scope must not BUILD the index: the full pass walks the real cache, but the
        // write guard at the bottom PUBLISHES the result, and once built, every later publication
        // feeds it through `note_sub_edges` — a staged class's edges would have to be in it for
        // the answers to be right, and they cannot be. The empty-map answer below keeps the
        // downward WALK (and `concrete_subtypes`) working over published + staged classes without
        // publishing anything; the index is still built on the first post-scope query.
        if dry_run_active() {
            return;
        }
        let mut map: crate::FastMap<Box<str>, Vec<Box<str>>> = Default::default();
        {
            let cache = self.cache_r();
            for (name, c) in cache.iter() {
                Self::push_edges(&mut map, name.as_str(), c);
            }
        }
        let mut w = self.sub_index.write().unwrap_or_else(|e| e.into_inner());
        if !w.0 {
            *w = (true, map);
        }
    }

    fn push_edges(
        map: &mut crate::FastMap<Box<str>, Vec<Box<str>>>,
        name: &str,
        c: &LoadedClass,
    ) {
        let child: Box<str> = Box::from(name);
        if let Some(sup) = c.file.super_class.as_deref() {
            map.entry(Box::from(sup)).or_default().push(child.clone());
        }
        for i in c.file.interfaces.iter() {
            map.entry(Box::from(i.as_str())).or_default().push(child.clone());
        }
    }

    /// Keep the downward index current at a class publication. No-op until the index has been
    /// built, so a run that never asks a hierarchy question pays one relaxed bool read per load.
    fn note_sub_edges(&self, name: &str, c: &LoadedClass) {
        let mut w = self.sub_index.write().unwrap_or_else(|e| e.into_inner());
        if !w.0 {
            return;
        }
        let (_, ref mut map) = *w;
        Self::push_edges(map, name, c);
    }

    /// Poisoning is not a mode this engine has: a panic inside a loader critical section aborts
    /// the whole run, so recovering the guard is always the right call and never hides a bug.
    fn cache_r(&self) -> std::sync::RwLockReadGuard<'_, crate::FastMap<String, Arc<LoadedClass>>> {
        self.cache.read().unwrap_or_else(|e| e.into_inner())
    }
    fn cache_w(&self) -> std::sync::RwLockWriteGuard<'_, crate::FastMap<String, Arc<LoadedClass>>> {
        self.cache.write().unwrap_or_else(|e| e.into_inner())
    }
    fn missing_r(&self) -> std::sync::RwLockReadGuard<'_, HashSet<Box<str>, std::hash::BuildHasherDefault<crate::FxHasher>>> {
        self.missing.read().unwrap_or_else(|e| e.into_inner())
    }
    fn missing_w(&self) -> std::sync::RwLockWriteGuard<'_, HashSet<Box<str>, std::hash::BuildHasherDefault<crate::FxHasher>>> {
        self.missing.write().unwrap_or_else(|e| e.into_inner())
    }
    fn bump_epoch(&self) {
        self.epoch.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
    fn sources_r(&self) -> std::sync::RwLockReadGuard<'_, Vec<Source>> {
        self.sources.read().unwrap_or_else(|e| e.into_inner())
    }
    fn sources_w(&self) -> std::sync::RwLockWriteGuard<'_, Vec<Source>> {
        self.sources.write().unwrap_or_else(|e| e.into_inner())
    }

    /// The classpath name index, built on first use and rebuilt when a source is added.
    ///
    /// Returns an `Arc` rather than a guard on purpose: every caller goes on to lock `sources`
    /// (and one of them a `Jar`), and handing out a guard would nest the two loader locks in the
    /// opposite order from the build below. The clone is one uncontended refcount against the
    /// forty lock acquisitions the index removes from the same call.
    ///
    /// `None` means "answer the old way": the kill switch is set, or a GIL-free worker asked —
    /// a worker publishes nothing process-wide, and this is shared state like any other.
    /// `g` is the caller's already-loaded [`loader_gates`] word: every caller of this goes on to
    /// read two or three more bits of it, and taking it as an argument is what makes those ONE
    /// load for the whole query instead of one acquire load apiece.
    fn path_index(&self, g: u32) -> Option<Arc<PathIndex>> {
        if !lg_of(g, LG_JARIO, fast_jario_live) {
            return None;
        }
        let want = self.sources_epoch.load(std::sync::atomic::Ordering::Acquire);
        {
            let g = self.path_index.read().unwrap_or_else(|e| e.into_inner());
            if let Some(ix) = g.as_ref() {
                if ix.epoch == want {
                    return Some(Arc::clone(ix));
                }
            }
        }
        if crate::parallel::gilfree() {
            return None;
        }
        let mut g = self.path_index.write().unwrap_or_else(|e| e.into_inner());
        if let Some(ix) = g.as_ref() {
            if ix.epoch == want {
                return Some(Arc::clone(ix));
            }
        }
        // Lock order is path_index -> sources, and it is the only place the two are nested;
        // `push_source` takes `sources` alone. The epoch is read UNDER the sources guard so the
        // built index and the counter it is tagged with describe the same source list.
        let built = {
            let sources = self.sources_r();
            let at = self.sources_epoch.load(std::sync::atomic::Ordering::Acquire);
            Arc::new(PathIndex::build(sources.as_slice(), at))
        };
        *g = Some(Arc::clone(&built));
        Some(built)
    }
    /// Every negative memo the classpath can invalidate, cleared in one place.
    /// Adding a source can make a previously-absent name resolve, which changes the answer
    /// to "what is this class's ancestry" as well as "does this class exist" — three call
    /// sites used to spell the same three clears out by hand and a fourth would have been a
    /// silently stale hierarchy.
    fn invalidate_for_new_source(&self) {
        self.missing_w().clear();
        self.super_chain_cache.write().unwrap_or_else(|e| e.into_inner()).clear();
        self.type_closure_cache.write().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// Names of all currently-loaded classes (for pre-warming per-class caches on the main thread
    /// before a GIL-free fork, so workers only READ them — never mutate the shared maps).
    /// ⭐Inside a dry-run scope the STAGED classes of this loader are included: the census asks
    /// "what could resolve" over published + staged, and a staged name is a parsed class this
    /// loader would answer. Names are order-stable (cache first, stage appended); the stage
    /// disappears with the scope.
    pub fn loaded_class_names(&self) -> Vec<String> {
        let mut out: Vec<String> = self.cache_r().keys().cloned().collect();
        if dry_run_active() {
            let id = self as *const ClassLoader as usize;
            DRY_RUN_LOADS.with_borrow(|s| {
                if let Some(staged) = s.as_ref().and_then(|sc| sc.stage_of(id)) {
                    out.extend(staged.keys().cloned());
                }
            });
        }
        out
    }

    /// The identity two classpath strings are the same ENTRY under. `canonicalize` resolves the
    /// symlinks, the `.`/`..` segments and (on Windows) the short-name and case spellings that
    /// make `C:\srv\libs\guava.jar` and the `file:/C:/srv/./libs/guava.jar` a `URLClassLoader`
    /// hands us the same file under two strings. It touches the filesystem — the same one stat
    /// the `is_dir`/`is_file` probe below already pays — and when it fails (the path does not
    /// exist, or is not reachable) the raw path stands in, which can only make the dedup MISS.
    /// A miss appends, which is the behaviour without this at all.
    fn source_key(p: &std::path::Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    }

    pub fn with_dir(mut self, dir: PathBuf) -> Self {
        self.source_paths.push(Self::source_key(&dir));
        self.push_source(Source::Dir(dir));
        self.invalidate_for_new_source();
        self.bump_epoch();
        self
    }

    pub fn with_jar(mut self, jar: Jar) -> Self {
        self.push_source(Source::Jar(std::sync::RwLock::new(jar)));
        self.invalidate_for_new_source();
        self.bump_epoch();
        self
    }

    /// Append a classpath source and move the source epoch, both under the SAME write guard.
    /// The pair has to be atomic with respect to `path_index`: a builder holding the read guard
    /// reads the epoch under it, and would otherwise be able to see the new source while still
    /// reading the old counter — publishing an index that outlives its own invalidation.
    fn push_source(&self, src: Source) {
        let mut g = self.sources_w();
        g.push(src);
        self.sources_epoch.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// Add a jar or directory to the classpath at runtime (URLClassLoader, the
    /// bundler's extracted server jar, etc.). Generic — no jar specifics.
    pub fn add_classpath(&mut self, path: &str) -> bool {
        let p = PathBuf::from(path);
        let fast = fast_superchain();
        // ⭐**AN ENTRY ALREADY ON THE CLASSPATH IS NOT A NEW SOURCE.** See [`fast_superchain`] for
        // why appending a second copy is both inert and expensive — a `Jar::open`, the merged
        // path index, and every memoised super-chain and type closure the boot has built so far.
        // `true` is the honest answer: the caller asked for the path to be on the classpath and
        // it is, which is exactly what the append it replaces would have reported.
        let key = if fast { Self::source_key(&p) } else { PathBuf::new() };
        if fast && self.source_paths.iter().any(|k| *k == key) {
            return true;
        }
        // ⭐**AND THE INVALIDATION MOVED BEHIND THE VERDICT.** It used to run FIRST, before the
        // path had been stat'd, so a URL naming nothing — or a jar whose central directory fails
        // to parse — still cleared the absent-name set and both hierarchy memos on its way to
        // returning `false`. Nothing had been added, so nothing could be stale; the clear was
        // pure loss. Ordering it push-then-invalidate matches `with_jar`/`with_dir`, and this
        // method takes `&mut self`, so no other thread can hold a `&ClassLoader` across the
        // window in which the source is visible and the memos are not yet cleared. With the
        // switch off, the original unconditional clear runs in its original position.
        if !fast {
            self.invalidate_for_new_source();
        }
        let added = if p.is_dir() {
            self.push_source(Source::Dir(p));
            true
        } else if p.is_file() {
            match Jar::open(&p) {
                Ok(jar) => {
                    self.push_source(Source::Jar(std::sync::RwLock::new(jar)));
                    true
                }
                Err(_) => false,
            }
        } else {
            false
        };
        if added {
            if fast {
                self.source_paths.push(key);
                self.invalidate_for_new_source();
            }
            // Recorded only for a source that actually joined the list, so a checkpoint restore
            // replays exactly the sources the original run held, in the same order — and replays
            // them through this same dedup, which can only skip a path the replay already holds.
            self.added_paths.push(path.to_string());
            self.bump_epoch();
        }
        added
    }

    /// SHARED-EXECUTION READ (`&self`): the cache-HIT half of [`Self::get_or_load`], and nothing
    /// else.
    ///
    /// The gil-free lane's ceiling is one global lock held across a callee's whole execution, and
    /// that lock exists to supply the exclusive `&mut Vm` the interpreter's signature demands —
    /// not because the work mutates. Measured: across a whole ring region and 292 million calls,
    /// workers touched frozen shared state twice. So the execution path can run on `&Vm` once the
    /// handful of `&mut` leaves grow `&self` twins, and this is the first of them.
    ///
    /// A MISS returns `None` rather than loading. That is not a new restriction: `get_or_load`
    /// already refuses to load on a gil-free worker — it requests the region abort and fails
    /// closed, because loading mutates the shared cache and moves the hierarchy epoch under every
    /// closure-based proof. This just expresses the same contract in the type.
    pub fn get_loaded(&self, name: &str) -> Option<Arc<LoadedClass>> {
        self.cache_r().get(name).map(Arc::clone)
    }

    /// Family routing fact captured when the class was loaded; this never loads the class.
    pub(crate) fn cached_native_family(
        &self,
        name: &str,
    ) -> Option<crate::owner_bits::NativeFamilyFact> {
        self.cache_r().get(name).map(|class| class.native_family)
    }

    /// Whether `name` is already known absent — the negative half of the same read.
    /// Kept separate so a caller can tell "not loaded yet" from "proven missing".
    pub fn known_missing(&self, name: &str) -> bool {
        self.missing_r().contains(name)
    }

    /// Get a class, parsing+caching on first request. Does NOT run `<clinit>`
    /// (the VM does, so it can execute bytecode).
    pub fn get_or_load(&self, name: &str) -> Result<Arc<LoadedClass>, LoadError> {
        // MIGRATED to the diagnostics registry: `RAVA_DIAG=loader` arms it, the dump names it
        // and prints its help line, and it appears on every dump trigger instead of only where
        // someone remembered to print it. The old `RAVA_LOOKUP_COUNT` still works — it feeds the
        // same static — so every measurement recorded against that name stays reproducible.
        // ⚠**RESOLVED ONCE.** `diag::selected` formats a string and scans the selector; this is
        // a 4.9-million-call path, so the verdict is cached — and now in the loader's shared gate
        // word rather than in a private `OnceLock`, so this reads a plain TLS word instead of an
        // acquire load in front of every class lookup in the engine.
        if lg(LG_COUNT_LOAD, count_load_live) {
            GET_OR_LOAD_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(c) = self.cache_r().get(name) {
            crate::contract_stat::class_lookup3(true, false);
            return Ok(Arc::clone(c));
        }
        // THIS VM'S OWN PLACEHOLDER NAMES ARE NOT CLASSES, AND ASKING FOR ONE COSTS A REGION.
        // `$Lambda` is the interned class of every lambda handle (`heap::lambdas`) and
        // `[arena-string]` that of every arena string: they name a REPRESENTATION, not a
        // classfile, and no classpath can supply them. On main the lookup merely wasted a
        // zip probe before being cached as absent, so nothing ever pointed at it. On a
        // GIL-free worker it fell through to the loader-miss arm below, which cannot load
        // and therefore tears down the whole region — measured, three times a boot, all of
        // them in the datapack reload (`[LANE-LOADER-MISS] worker asked for "$Lambda"`).
        // Answering NotFound here is what the serial path already concluded, one zip probe
        // later, and it is a fact about this engine's naming, not about any workload.
        if matches!(name, "$Lambda" | "[arena-string]") {
            return Err(LoadError::NotFound(std::sync::Arc::from(name)));
        }
        // ⛔REVERTED, AND MEASURED BOTH WAYS. This path is taken **4,512,560 times a boot**
        // against 4,499 classes actually parsed, and it builds a fresh name for an error
        // the caller discards — so removing that allocation looked free. Replacing it with
        // an `Arc` clone made the boot SLOWER, twice: wall −0.68% then −0.11%, done −1.78%
        // then −1.19%, alloc faster in 5 of 6 pairs both times (ledger row 284).
        //
        // The likely reason is that the saving was never the allocation: a small
        // allocation is thread-local in mimalloc, while cloning an `Arc` out of a SHARED
        // set is an atomic refcount on one cache line, 4.5 million times. Trading an
        // allocation for synchronisation is not a trade this workload wants.
        //
        // ⛔A THREAD-LOCAL NEGATIVE CACHE WAS TRIED HERE TOO, AND ALSO REVERTED. If the
        // allocation is not the cost, the `RwLock` read is the next candidate: 4.5M
        // acquisitions of a lock nobody contends. A thread-local set answers without one,
        // and it read +0.96% (1 of 6 pairs against) in the first batch and +0.34% (3 of 6)
        // in the second — sign steady, magnitude gone, win rate a coin flip (row 285).
        //
        // ⭐So the population is enormous and each probe is genuinely cheap. The remaining
        // question is not what a probe costs. It is why the caller walks `ArrayList`'s
        // super and interface closure ~310,000 times a boot: the absent names are
        // `java/lang/Boolean` (879,575), `java/lang/Object` (600,000), then `Comparable` /
        // `Serializable` / `Cloneable` / `Iterable` / `Collection` / `List` / `RandomAccess`
        // / `AbstractCollection` at ~310,000 each. Those are not random misses; that is one
        // hierarchy walk, repeated.
        //
        // (The reverted cache had a defect worth keeping in mind for the next one: nothing
        // cleared it on `reset`, so a class that later became loadable would still read
        // absent on a thread that had cached it. A cache with no invalidation story is a
        // bug with a schedule.)
        if self.missing_r().contains(name) {
            crate::contract_stat::class_lookup3(false, true);
            crate::contract_stat::class_absent_name(name);
            crate::contract_stat::class_absent_site();
            return Err(LoadError::NotFound(std::sync::Arc::from(name)));
        }
        // ⭐**THE DRY-RUN ARMS — THE SCOPE'S ENTIRE CONTRACT, CHECKED IN ONE TLS LOAD.** A miss
        // with a scope open parses to the stage or fails negative without touching ANY shared
        // map. Placed AFTER the real-cache and missing checks (published facts stay readable and
        // authoritative; an already-proven-absent name stays absent).
        if dry_run_active() {
            // ⛔**A GIL-FREE WORKER FAILS CLOSED EVEN INSIDE A SCOPE.** The parse-and-stage
            // below is exactly the shared-read work the worker contract refuses, so the scope
            // must not arm it on a worker: request the region abort and return `WorkerRefused`
            // BEFORE any staging. (This arm sits ahead of the serial gilfree refusal below only
            // in source order; this explicit check is what keeps a worker out.)
            if crate::parallel::gilfree() {
                crate::parallel::request_abort();
                return Err(LoadError::WorkerRefused(name.to_string()));
            }
            let id = self as *const ClassLoader as usize;
            // One borrow for this arm. `read_bytes_uncached` reads the source list and archive
            // entries but never re-enters this TLS scope or mutates loader state.
            return DRY_RUN_LOADS.with_borrow_mut(|slot| {
                let Some(scope) = slot.as_mut() else { unreachable!("checked active") };
                let stage = scope.stage_for(id);
                if let Some(staged) = stage.get(name) {
                    // A staged lookup is REPEATABLE: the same Arc, no re-parse, no cache write.
                    return Ok(Arc::clone(staged));
                }
                match self.read_bytes_uncached(name) {
                    Ok(bytes) => {
                        let file =
                            ClassFile::parse(&bytes).map_err(|e| LoadError::Parse(e.to_string()))?;
                        let loaded = LoadedClass::new(file);
                        stage.insert(name.to_string(), Arc::clone(&loaded));
                        Ok(loaded)
                    }
                    // The negative answer STANDS for the rest of the scope (a real miss would
                    // have recorded it in `missing`; the stage records nothing, so the same
                    // question re-scans — correct, if slower) and touches no shared set.
                    Err(e) => Err(e),
                }
            });
        }
        crate::contract_stat::class_lookup3(false, false);
        // GAP-S07/S09: a GIL-FREE WORKER must never LOAD a class — loading mutates the
        // shared cache and moves the hierarchy epoch under every closure-based proof.
        // A cache miss on such a thread requests the region abort (the coordinator
        // discards the region and retries serially) and fails closed; the cache HITS
        // above only read entries published before the region opened.
        if crate::parallel::gilfree() {
            // NAME THE MISS (RAVA_LANE_ABORT_SITE). "A worker reached the loader" has two very
            // different causes and the counter cannot tell them apart: a genuinely cold class the
            // serial warm-up never needed, or a torn read handing us a name that was never a class
            // at all. The string decides it — and it decided the no-lock experiment, where eight
            // serial pilots left the count unchanged.
            #[cfg(feature = "next-gen-gc")]
            if crate::parallel::lane_abort_site_enabled() {
                static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 12 {
                    eprintln!("[LANE-LOADER-MISS] worker asked for {name:?}");
                }
            }
            crate::parallel::request_abort();
            // NOT NotFound: the class may well be on the classpath. Callers that cannot tell the
            // two apart turn a region abort into a fatal "unsupported native" (see the variant's
            // own documentation).
            return Err(LoadError::WorkerRefused(name.to_string()));
        }
        // 20.3 wall-profile: a cache MISS — the actual read (zip inflate) + parse/link work.
        let _wp = crate::rava_sampler::enter(crate::rava_sampler::CLASS_LOADING);
        let bytes = match self.read_bytes(name) {
            Ok(b) => b,
            Err(e @ LoadError::NotFound(_)) => {
                self.missing_w().insert(name.into());
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        let file = ClassFile::parse(&bytes).map_err(|e| LoadError::Parse(e.to_string()))?;
        let loaded = LoadedClass::new(file);
        crate::pilot_census::note_class_load(name);
        // A real classfile for a name the natives would otherwise claim takes precedence over
        // the native — this is the hook the `java.util`-as-bytecode migration turns on.
        crate::class_loader::note_bytecode(name);
        self.cache_w().insert(name.to_string(), Arc::clone(&loaded));
        self.note_sub_edges(name, &loaded);
        self.bump_epoch(); // GAP-S06/S02: the loaded-class set changed
        Ok(loaded)
    }

    /// Insert a SYNTHESIZED class (tests/tooling that build `ClassFile`s in memory —
    /// mirrors `ClassFile::synthesize`). The class behaves exactly like a parsed one;
    /// the name key is `file.this_class`. Negative caches are cleared so previously
    /// missing names can now resolve.
    ///
    /// GAP-S08: one textual name maps to exactly ONE class identity for the process
    /// lifetime — an already-resolved name is returned unchanged, never replaced, so
    /// no code path (production or test) can alias or swap a published class.
    pub fn define_synthetic(&self, file: ClassFile) -> Arc<LoadedClass> {
        let name = file.this_class.clone();
        if let Some(existing) = self.cache_r().get(&name) {
            return Arc::clone(existing);
        }
        let loaded = LoadedClass::new(file);
        self.note_sub_edges(&name, &loaded);
        self.cache_w().insert(name, Arc::clone(&loaded));
        // A name that was absent can now resolve, and every hierarchy answer derived from it
        // is stale — exactly the invalidation a new classpath entry needs.
        self.invalidate_for_new_source();
        self.bump_epoch(); // GAP-S06/S02: the loaded-class set changed
        loaded
    }

    /// Already-cached lookup without loading (used to test availability).
    pub fn cached(&self, name: &str) -> Option<Arc<LoadedClass>> {
        self.cache_r().get(name).map(Arc::clone)
    }

    /// `start`'s name + its superclass-chain names (readable even when a super
    /// isn't loadable — each class file records its super's name), ending at
    /// Object. Memoised: the result is stable until a source is added.
    /// ⛔ A PER-THREAD FRONT CACHE HERE WAS BUILT AND REFUTED (2026-07-29). The growth-ranked lane
    /// profile (4 vs 6 workers) puts `natives::dispatch_owner` at 0% -> 2.85%, `try_invoke_chain`
    /// 0% -> 2.70% and `jit_native_served_deep` 0% -> 2.82% - all of which reach this function -
    /// beside `RtlWaitOnAddress` at 5.06%, and `RwLock::read()` is an atomic RMW on ONE shared
    /// word, so it looked like the contended line. It is not, at this width: an epoch-keyed
    /// thread-local mirror measured a small REGRESSION on a clean six-rep batch - Done means 22.33
    /// vs 22.16, wall means 68.93 vs 68.08, and base ahead on both minima too.
    ///
    /// Fifth instance of the same failure mode in one session: a cache in front of an
    /// already-short path is a tax. The shared read is cheap because the reader count is rarely
    /// contended - four workers, and the ring is only part of the boot. Do not re-derive it from
    /// the growth profile.
    pub(crate) fn super_chain(&self, start: &str) -> Arc<[Box<str>]> {
        // ⭐**A CACHE KEYED ON A STRING STILL HASHES THE STRING EVERY CALL.** The hit rate here is
        // effectively 100% (a boot loads ~10k classes), so what is paid per call is an RwLock read
        // plus a hash of a class name like
        // `net/minecraft/world/level/block/state/BlockBehaviour$BlockStateBase`. Count the calls
        // before assuming that matters — counting is immune to the box drift that currently puts
        // the timing floor at ~8%.
        // ⚠**RESOLVED ONCE, AND ONCE FOR ALL THREE.** This opened with an `OnceLock` that only
        // decides whether a DIAGNOSTIC counter ticks, then read two more (`fast_classload` for the
        // splice, `fast_loader3` INSIDE the ancestor walk) — three acquire loads per call on a
        // path taken once per class the boot links, plus one per ancestor step.
        let g = loader_gates();
        if lg_of(g, LG_COUNT_SUPER, count_super_live) {
            SUPER_CHAIN_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(c) = self.super_chain_cache.read().unwrap_or_else(|e| e.into_inner()).get(start) {
            return Arc::clone(c);
        }
        // ⭐**AN ANCESTOR'S CHAIN IS THE TAIL OF THIS ONE — SPLICE IT, DO NOT RE-WALK IT.**
        // The memo answers a class only if that exact class was asked for before, so a boot
        // that loads ~15,000 classes walked ~15,000 chains, and every one of them re-derived
        // the same shared spine (`BlockBehaviour$BlockStateBase` -> ... -> `Object`) with a
        // `get_or_load` and a `String` rebuild PER ANCESTOR. Superclasses are asked about
        // before their subclasses in practice — a class cannot link before its super — so the
        // tail is almost always already memoised, and the walk stops at the first hit.
        //
        // This is a per-class ONE-SHOT cost the JIT can never amortise, which is why it is
        // worth removing from the path rather than making each step cheaper.
        let splice = lg_of(g, LG_CLASSLOAD, crate::class_loader::fast_classload_live);
        // Hoisted out of the walk: the cycle guard below is read once per ANCESTOR STEP.
        let cycle_guard = lg_of(g, LG_LOADER3, fast_loader3_live);
        let mut names: Vec<Box<str>> = vec![start.into()];
        let mut current = start.to_string();
        // A GIL-free worker cannot load, so its walk stops early — and a chain that stopped early
        // is not the class's hierarchy, it is a fragment. Publishing it would hand the SERIAL path
        // a truncated ancestry as authoritative, and every consumer of this cache resolves against
        // it: the inherited-static walk in putstatic, field and method resolution, subtype tests.
        // Silent wrong answers, on the shipped JIT-on configuration, because a worker happened to
        // ask first.
        let mut refused = false;
        let mut first = true;
        loop {
            // `current` is always the name last pushed onto `names`, so a memo hit contributes
            // its entries from index 1 — index 0 IS `current`. Only COMPLETE chains are ever
            // published (a refused walk is never cached), so a spliced chain is complete too
            // and stays cacheable. Skipped on the first iteration: `start` was already missed
            // above, and re-probing would be a second hash of the same name.
            if splice && !first {
                let hit = self
                    .super_chain_cache
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(current.as_str())
                    .map(Arc::clone);
                if let Some(anc) = hit {
                    for n in anc.iter().skip(1) {
                        if !names.iter().any(|x| x.as_ref() == n.as_ref()) {
                            names.push(n.clone());
                        }
                    }
                    break;
                }
            }
            first = false;
            match self.get_or_load(&current) {
                Ok(c) => match c.file.super_class.clone() {
                    Some(sup) => {
                        // ⛔**THE DEDUP WAS NOT A TERMINATION GUARD, AND THE WALK HAD NONE.** A
                        // name already on the chain was not pushed again — and then followed
                        // anyway, so `A extends B extends A` spun here forever, appending
                        // nothing, with no depth bound and no `seen` set (`type_closure`'s walk
                        // has one; this one relied on the hierarchy being acyclic). A repeated
                        // super IS the cycle, and JVMS §5.3.5 makes it a ClassCircularityError
                        // rather than a hierarchy, so the chain ends here: callers get the
                        // ancestry walked so far, which is what a `break` on an unloadable super
                        // already hands them.
                        if cycle_guard && names.iter().any(|n| n.as_ref() == sup) {
                            break;
                        }
                        if !names.iter().any(|n| n.as_ref() == sup) {
                            names.push(sup.as_str().into());
                        }
                        current = sup;
                    }
                    None => break,
                },
                Err(LoadError::WorkerRefused(_)) => {
                    refused = true;
                    break;
                }
                Err(_) => break, // JDK class not in jar — name already recorded
            }
        }
        if !names.iter().any(|n| n.as_ref() == "java/lang/Object") {
            names.push("java/lang/Object".into());
        }
        let arc: Arc<[Box<str>]> = names.into();
        // Only a chain walked to completion is cacheable. The refused walk still returns what it
        // has — the worker's region is already aborting (the loader requested it) and the caller
        // is about to unwind — but it never becomes the process-wide answer.
        // ⛔Nor a DRY-RUN walked one: the walk's `get_or_load`s were served from the stage (and
        // may differ from what the published set would answer), so the memo must not outlive the
        // scope. Read-only hits above stay sound: a pre-scope memo entry is a fact of published
        // classes and every dry walk consults them as such.
        if !refused && !dry_run_active() {
            self.super_chain_cache
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(start.into(), Arc::clone(&arc));
        }
        arc
    }

    /// EVERY NAME THIS CLASS IS: its superclass chain and its transitive interfaces.
    ///
    /// A sampled-backtrace census (1 stack per 10,000 absent class probes) attributed
    /// **353 of 450 samples — 78.4% — to a single caller**: `structural_key_scoped` →
    /// `drain_map` → `implements_iface`. That path asks "is this object a `java/util/Map`?"
    /// while deriving a map key, i.e. for essentially every object, and the walk was an
    /// uncached BFS: a `String` per node, a linearly-scanned `Vec<String>` for `seen`, and
    /// a loader lookup per node. A NEGATIVE answer is the expensive one, because proving
    /// "not a Map" means visiting the whole closure — which is exactly why the absent-name
    /// tally read as `ArrayList`'s ancestry (`Comparable` / `Serializable` / `Cloneable` /
    /// `Iterable` / `Collection` / `List` / `RandomAccess` / `AbstractCollection`, ~310,000
    /// each). Those names were never the point. One repeated walk was.
    ///
    /// Rows 284 and 285 both lost by attacking the probe; this attacks the WALK. It also
    /// answers every target from one memo, since the closure does not depend on what is
    /// being asked about.
    ///
    /// Invalidation matches `super_chain`, for the same reason: a chain that stopped early
    /// because a GIL-free worker may not load is a FRAGMENT, and publishing a fragment as
    /// the process-wide answer to a subtype question is a silent wrong answer on the
    /// shipped configuration. A refused walk returns what it has and is never cached.
    /// `RAVA_NO_TYPE_CLOSURE=1` falls back to walking every time, so the saving is priced.
    pub(crate) fn type_closure(&self, start: &str) -> Arc<crate::FastSet<Box<str>>> {
        // Two process constants per call (`RAVA_NO_TYPE_CLOSURE` and `fast_classload`), read as
        // two acquire loads on the path every subtype test takes; one word now answers both.
        let g = loader_gates();
        let on = lg_of(g, LG_TYPE_CLOSURE, type_closure_live);
        if on {
            if let Some(c) =
                self.type_closure_cache.read().unwrap_or_else(|e| e.into_inner()).get(start)
            {
                return Arc::clone(c);
            }
        }
        // ⭐**A CLOSURE IS THE UNION OF ITS DIRECT PARENTS' CLOSURES, AND THOSE ARE MEMOISED.**
        // The walk below is breadth-first over the WHOLE hierarchy: one `get_or_load` and one
        // `Box<str>` per node, every time a class is asked about for the first time. But a type
        // closure is transitively closed by construction, so `closure(C) = {C} ∪ closure(super)
        // ∪ closure(each direct interface)` — which touches only C's direct parents and reads
        // the rest out of the memo. It also FILLS the memo for every ancestor on the way up, so
        // a hierarchy is walked once in total instead of once per class that stands in it, and
        // the shared spine (`Object`, `Comparable`, `Serializable`, `Iterable`, `Collection`,
        // `List`, ... — the names the absent-probe tally ranked at ~310,000 each) is derived
        // exactly once.
        //
        // Only on the serial path: `get_or_load` can refuse for a GIL-free worker, and a
        // fragment published as the process-wide answer to a subtype question is a silent wrong
        // answer (see the doc above). Recursion is bounded; a malformed cycle falls back here.
        //
        // ⛔And never during a DRY-RUN scope: `closure_via_parents` FILLS the shared memo for
        // every ancestor on the way up, and those closures were derived over staged (never
        // published) classes — publishing them would outlive the scope and change later answers.
        // The breadth-first walk below serves the same verdict from the stage instead.
        if on
            && lg_of(g, LG_CLASSLOAD, crate::class_loader::fast_classload_live)
            && !crate::parallel::gilfree()
            && !dry_run_active()
        {
            if let Some(a) = self.closure_via_parents(start, 0) {
                return a;
            }
        }
        // A SET, not a list: the caller's question is membership ("is this a
        // `java/util/Map`?"), and a list turned that into up to fourteen string compares
        // per ask. It also subsumes the `seen` bookkeeping the old walk did with a
        // linearly-scanned `Vec<String>`.
        let mut names: crate::FastSet<Box<str>> = crate::FastSet::default();
        let mut queue: Vec<Box<str>> = vec![start.into()];
        let mut refused = false;
        while let Some(cn) = queue.pop() {
            if !names.insert(cn.clone()) {
                continue;
            }
            match self.get_or_load(&cn) {
                Ok(c) => {
                    if let Some(sup) = c.file.super_class.as_deref() {
                        queue.push(sup.into());
                    }
                    queue.extend(c.file.interfaces.iter().map(|i| Box::from(i.as_str())));
                }
                Err(LoadError::WorkerRefused(_)) => {
                    refused = true;
                    break;
                }
                // A JDK class with no bytecode in any source: the name is already recorded
                // and its own ancestry is served natively, so there is nothing to walk.
                Err(_) => {}
            }
        }
        let arc: Arc<crate::FastSet<Box<str>>> = Arc::new(names);
        // ⛔Same gate as the walk: a dry-run closure was derived over staged classes and must not
        // become the process-wide answer. Read-only memo hits above stay sound.
        if on && !refused && !dry_run_active() {
            self.type_closure_cache
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(start.into(), Arc::clone(&arc));
        }
        arc
    }

    /// `type_closure`'s fast form: `{start}` united with each DIRECT parent's closure, taken
    /// from the memo when it is there and computed (and memoised) when it is not.
    ///
    /// Correctness rests on one property of the set it builds: a type closure is TRANSITIVELY
    /// CLOSED, so it equals the breadth-first walk's result exactly, and if a parent's name is
    /// already present then that parent's whole closure is too — which is why an already-covered
    /// parent may be skipped rather than recursed into.
    ///
    /// Returns `None`, never a partial answer, in the two cases it must not decide:
    ///   * deeper than any real hierarchy (a malformed `super`/interface cycle would otherwise
    ///     recurse until the stack ends), and
    ///   * a `WorkerRefused` load, which the caller's gate already makes unreachable but which
    ///     would publish a FRAGMENT as the process-wide subtype answer if it ever were not.
    /// The caller then runs the breadth-first walk, which carries its own `seen` set.
    ///
    /// A refusal never publishes a fragment: the memo insert is the last statement, reached only
    /// after every parent has answered in full. Entries a deeper level wrote before an ancestor
    /// above it hit the depth limit are therefore complete closures in their own right, and the
    /// breadth-first fallback reads them back as memo hits.
    fn closure_via_parents(
        &self,
        start: &str,
        depth: u32,
    ) -> Option<Arc<crate::FastSet<Box<str>>>> {
        // 64 is well past the deepest real hierarchy (MC's worst is ~15) and well short of the
        // stack a recursive descent of `Box<str>` frames can use.
        if depth > 64 {
            return None;
        }
        if let Some(c) = self.type_closure_cache.read().unwrap_or_else(|e| e.into_inner()).get(start)
        {
            return Some(Arc::clone(c));
        }
        let mut names: crate::FastSet<Box<str>> = crate::FastSet::default();
        names.insert(start.into());
        match self.get_or_load(start) {
            Ok(c) => {
                // Snapshot the direct parents before recursing: the recursion takes the memo's
                // write lock, and holding a borrow of the class across it buys nothing.
                let mut parents: Vec<Box<str>> = Vec::with_capacity(c.file.interfaces.len() + 1);
                if let Some(sup) = c.file.super_class.as_deref() {
                    parents.push(sup.into());
                }
                parents.extend(c.file.interfaces.iter().map(|i| Box::<str>::from(i.as_str())));
                for p in parents {
                    // Already in the set means an earlier parent's closure contained it, and a
                    // closure containing a name contains that name's closure. Nothing to add.
                    if names.contains(&p) {
                        continue;
                    }
                    names.extend(self.closure_via_parents(&p, depth + 1)?.iter().cloned());
                }
            }
            Err(LoadError::WorkerRefused(_)) => return None,
            // A JDK class with no bytecode in any source: its ancestry is served natively and
            // there is nothing to walk, so the class is its own closure — the same answer the
            // breadth-first walk records, and the same one it caches.
            Err(_) => {}
        }
        let arc: Arc<crate::FastSet<Box<str>>> = Arc::new(names);
        self.type_closure_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(start.into(), Arc::clone(&arc));
        Some(arc)
    }

    /// ⭐**IS THIS CLASS A NETTY EVENT EXECUTOR — one `type_closure` read, nothing new.**
    ///
    /// The `io/netty/` prefix arm of `force_native_collection`'s `inEventLoop` route
    /// is a name approximation of a structural fact: the ask is sound exactly for
    /// classes whose type closure contains `EventExecutorGroup` (the census-proven
    /// 22-class family). This is that structural fact, read from the ALREADY-memoised,
    /// ALREADY-invalidated per-loader closure — no new cache, no new invalidation, no
    /// owner_bits involvement (the FACTS word is name-pure and shared across loaders;
    /// this answer is neither).
    ///
    /// Loader identity and staleness are inherited by construction: `type_closure` is
    /// keyed per `ClassLoader`, cleared by `invalidate_for_new_source`, and never
    /// publishes a GIL-free worker's fragment — a fragment can only produce an
    /// inconclusive FALSE here, never a wrong TRUE.
    pub(crate) fn is_netty_executor_family(&self, class: &str) -> bool {
        self.type_closure(class).contains(crate::net::EVENT_EXECUTOR_GROUP_ROOT)
    }

    /// Every static-field value across all loaded classes — GC roots, since a
    /// static can hold a live object/array reference.
    pub fn static_roots(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for c in self.cache_r().values() {
            out.extend(c.statics.read().unwrap_or_else(|e| e.into_inner()).values().cloned());
        }
        out
    }

    /// Apply `f` to every static field value MUTABLY — the moving GC uses this to
    /// forward arena pointers held in statics (a root source). Called only at a
    /// safepoint (no concurrent static access).
    ///
    /// ⚠ `f` MUST NOT read or write statics. This is the one place the write guard is held
    /// across a call rather than for a single expression, and since `statics` became an
    /// `RwLock` the failure mode changed: a `RefCell` would have PANICKED on the re-entrant
    /// borrow, where an `RwLock` DEADLOCKS and simply hangs the boot with no message. The
    /// safepoint makes concurrent access impossible; this note covers re-entrant access,
    /// which the safepoint does not.
    #[cfg(feature = "next-gen-gc")]
    pub fn forward_statics(&self, f: &mut dyn FnMut(&mut Value)) {
        for c in self.cache_r().values() {
            for v in c.statics.write().unwrap_or_else(|e| e.into_inner()).values_mut() {
                f(v);
            }
        }
    }

    /// Snapshot every loaded class's static fields (class name → field → value),
    /// for checkpoint/restore. Static values may hold heap handles — valid only
    /// alongside the matching `Heap::snapshot` (same handle space).
    pub fn snapshot_statics(&self) -> Vec<(String, crate::FastMap<String, Value>)> {
        // Every INITIALIZED class (not just those with non-empty statics): on
        // restore each is marked initialized so its <clinit> never re-runs (which
        // would re-fire side effects / reset statics). Empty-statics initialized
        // classes still need the flag.
        self.cache_r()
            .iter()
            .filter(|(_, c)| c.initialized.load(std::sync::atomic::Ordering::Relaxed))
            .map(|(name, c)| (name.clone(), c.statics.read().unwrap_or_else(|e| e.into_inner()).clone()))
            .collect()
    }

    /// Re-inject snapshotted statics: reload each class from its source (does NOT
    /// run `<clinit>`) and overwrite its static map. Pair with `Heap::restore`.
    pub fn restore_statics(
        &mut self,
        snap: Vec<(String, crate::FastMap<String, Value>)>,
    ) {
        for (name, statics) in snap {
            if let Ok(c) = self.get_or_load(&name) {
                *c.statics.write().unwrap_or_else(|e| e.into_inner()) = statics;
                // Already initialized in the checkpointed run — don't let a later
                // getstatic re-run <clinit> and overwrite the restored statics.
                c.initialized.store(true, std::sync::atomic::Ordering::Relaxed);
                c.clinit_done.store(true, std::sync::atomic::Ordering::Relaxed); // statics restored = clinit effectively complete
            }
        }
    }

    /// Read a non-class resource (by archive/relative path) from the dir or jar.
    pub fn read_resource(&self, path: &str) -> Option<Vec<u8>> {
        // ⭐**IS THERE ANYTHING TO CACHE?** This scans the whole classpath per call — a `is_file`
        // syscall per directory source, then a probe per jar — and the histogram prices the
        // resulting `Files.newInputStream` at **92.7 us over 6,308 calls (0.58 s)** with zero
        // guest opcodes inside. A cache only pays if paths REPEAT, so count that before building
        // one: MC's loader may call `exists` / `size` / `newInputStream` for the same resource.
        // ⭐**ONE WORD FOR THE WHOLE QUERY.** This method read four different `OnceLock`s —
        // `RAVA_RES_STAT`, `fast_jario` (through `path_index`), `fast_loader3` and
        // `fast_classload` — plus one per binary-search probe inside `exact`. All of them are
        // process constants; the word resolves them once per thread and is passed down.
        let g = loader_gates();
        if lg_of(g, LG_RES_STAT, res_stat_live) {
            use std::sync::atomic::Ordering::Relaxed;
            RES_LOOKUPS.fetch_add(1, Relaxed);
            let mut seen = RES_SEEN.lock().unwrap_or_else(|e| e.into_inner());
            let set = seen.get_or_insert_with(std::collections::HashSet::new);
            if set.insert(path.to_string()) {
                RES_DISTINCT.fetch_add(1, Relaxed);
            }
        }
        // ⭐**ASK THE INDEX, THEN THE ARCHIVE.** `read_entry` needs `&mut` and used to be the
        // ONLY way to find out whether a jar even has the path, so a scan across the bundled
        // server jar plus every `add_classpath` library took an exclusive lock per source and
        // held it across the inflate. `has_entry` is a `HashSet` probe on `&self`, so the
        // misses are shared reads and only the jar that answers is ever locked exclusively.
        //
        // The CLASSPATH index goes one further: the shared read and the hash are still forty of
        // them, and the answer is already known. `owners` is the run of sources holding this
        // exact name (usually zero or one), so a jar is ruled out by an integer compare and a
        // path no jar holds skips every one of them without touching a lock.
        let ix = self.path_index(g);
        let owners = ix.as_ref().map(|i| i.exact(path));
        // ⭐**A NAME NO JAR HOLDS, ON A JARS-ONLY CLASSPATH, IS ALREADY ANSWERED.** The index
        // covers every jar, so an empty `owners` with no directory source left is a final NO —
        // and it is the DOMINANT case here. The loop below would still take the `sources` read
        // guard and step over all ~40 entries to reach it, which is O(classpath) per query for a
        // question the binary search already decided.
        if lg_of(g, LG_LOADER3, fast_loader3_live)
            && owners.is_some_and(|o| o.is_empty())
            && ix.as_ref().is_some_and(|i| !i.has_dirs)
        {
            return None;
        }
        let fast = lg_of(g, LG_CLASSLOAD, crate::class_loader::fast_classload_live);
        for (i, src) in self.sources_r().iter().enumerate() {
            if let (Some(o), Source::Jar(_)) = (owners, src) {
                if !o.iter().any(|e| e.src == i as u32) {
                    continue;
                }
            }
            match src {
                Source::Dir(dir) => {
                    // ONE filesystem round trip, not two. `is_file()` and `read()` asked the
                    // same question, and on Windows each is a real kernel transition; a read
                    // that fails because the path is absent or is a directory is the same
                    // answer `is_file()` gave, so keep scanning.
                    let p = dir.join(path);
                    if !fast {
                        if p.is_file() {
                            return std::fs::read(&p).ok();
                        }
                        continue;
                    }
                    if let Ok(bytes) = std::fs::read(&p) {
                        return Some(bytes);
                    }
                }
                Source::Jar(jar) => {
                    // The index already proved this jar holds the name; re-probing it would be
                    // the shared read this whole path exists to remove.
                    if owners.is_none()
                        && fast
                        && !jar.read().unwrap_or_else(|e| e.into_inner()).has_entry(path)
                    {
                        continue;
                    }
                    if let Ok(Some(bytes)) =
                        jar.write().unwrap_or_else(|e| e.into_inner()).read_entry(path)
                    {
                        return Some(bytes);
                    }
                }
            }
        }
        None
    }

    /// One directory source's subtree under `prefix`, as loader-relative slash paths.
    ///
    /// Kept out of the path index deliberately: a directory's contents are NOT immutable while
    /// the server runs, so an indexed listing would be a cached answer the filesystem can
    /// invalidate underneath us. Jars are fixed at `Jar::open`; directories are walked live.
    fn dir_entries_under(
        dir: &std::path::Path,
        prefix: &str,
        out: &mut std::collections::BTreeSet<String>,
    ) {
        let mut stack = vec![dir.join(prefix)];
        while let Some(d) = stack.pop() {
            if let Ok(rd) = std::fs::read_dir(&d) {
                for e in rd.flatten() {
                    let p = e.path();
                    if let Ok(rel) = p.strip_prefix(dir) {
                        out.insert(rel.to_string_lossy().replace('\\', "/"));
                    }
                    if p.is_dir() {
                        stack.push(p);
                    }
                }
            }
        }
    }

    /// All resource paths under `prefix`, across jars and dirs, deduplicated.
    /// Used to enumerate a jar-internal resource tree (e.g. the `data/` pack).
    ///
    /// ⭐**THE LISTING NOW COSTS ITS OWN SIZE, NOT THE ARCHIVE'S.** `Jar::entries_under` is a
    /// linear scan of one jar's entire name list that clones every match, so this walked every
    /// entry name of every jar — ~100,000 `starts_with` calls and a `BTreeSet<String>` rebuilt
    /// from scratch — for each `Files.list` / `Files.walk` / `Files.find`, and MC's pack scan
    /// asks per directory per namespace per registry. The index's matches are one CONTIGUOUS
    /// SORTED RUN, found by binary search: O(log N + k) for a k-entry answer.
    pub fn list_resources(&self, prefix: &str) -> Vec<String> {
        // One word for every gate this query reads — see [`loader_gates`].
        let g = loader_gates();
        if let Some(ix) = self.path_index(g) {
            let run = ix.under(prefix);
            // A dir source needs the live walk, and merging two orders needs the tree. With no
            // dir source on the path (the shipped MC classpath is jars only) the run is already
            // sorted, so adjacent dedup IS full dedup and the `BTreeSet` disappears entirely.
            let has_dir = if lg_of(g, LG_LOADER3, fast_loader3_live) {
                ix.has_dirs
            } else {
                self.sources_r().iter().any(|s| matches!(s, Source::Dir(_)))
            };
            if !has_dir {
                let mut out: Vec<String> = Vec::with_capacity(run.len());
                for e in run {
                    let n = ix.name(e);
                    if out.last().map(String::as_str) != Some(n) {
                        out.push(n.to_string());
                    }
                }
                return out;
            }
            let mut out: std::collections::BTreeSet<String> =
                run.iter().map(|e| ix.name(e).to_string()).collect();
            for src in self.sources_r().iter() {
                if let Source::Dir(dir) = src {
                    Self::dir_entries_under(dir, prefix, &mut out);
                }
            }
            return out.into_iter().collect();
        }
        let mut out = std::collections::BTreeSet::new();
        for src in self.sources_r().iter() {
            match src {
                Source::Jar(jar) => out.extend(
                    jar.read().unwrap_or_else(|e| e.into_inner()).entries_under(prefix),
                ),
                Source::Dir(dir) => Self::dir_entries_under(dir, prefix, &mut out),
            }
        }
        out.into_iter().collect()
    }

    /// The DIRECT children of `prefix` (which must end in `/`), one level deep, as full paths —
    /// what `Files.list` / `Files.newDirectoryStream` actually ask for.
    ///
    /// ⭐**ONE LEVEL SHOULD NOT COST THE WHOLE SUBTREE.** The caller built this from
    /// `list_resources`, i.e. it materialised every DESCENDANT (`data/` is thousands of entries),
    /// kept the first path segment of each and threw the rest away — and it does that once per
    /// directory node as it walks down, so a tree of depth d costs its own size d times. Over the
    /// index a child is found and then SKIPPED PAST with a binary search over its whole subtree,
    /// so the cost is one search per child returned.
    ///
    /// The result is a `BTreeSet` for the same reason the caller's was: sorting is not enough to
    /// make the children unique. `.` (0x2E) sorts below `/` (0x2F), so `data/foo`, `data/foo.json`
    /// and `data/foo/x` interleave, and the segment `foo` appears twice non-adjacently.
    pub fn list_resource_children(&self, prefix: &str) -> Vec<String> {
        let mut out: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        // One word for every gate this query reads — see [`loader_gates`].
        let g = loader_gates();
        // ⭐**AND HOISTED OUT OF THE CHILD WALK.** The loop below runs once per child of
        // `prefix` — a `data/` namespace scan asks it per directory node — and read the gate
        // INSIDE it, so a listing with k children took k acquire loads for one process constant.
        let l3 = lg_of(g, LG_LOADER3, fast_loader3_live);
        if let Some(ix) = self.path_index(g) {
            let run = ix.under(prefix);
            let mut i = 0usize;
            while i < run.len() {
                let n = ix.name(&run[i]);
                let rest = &n[prefix.len().min(n.len())..];
                let first = rest.trim_end_matches('/').split('/').next().unwrap_or("");
                if first.is_empty() {
                    i += 1;
                    continue;
                }
                let child = format!("{prefix}{first}");
                let tail = &run[i..];
                if l3 {
                    // ⛔**"THIS CHILD'S SUBTREE IS ONE CONTIGUOUS RUN" IS FALSE, AND SO WAS THE
                    // PREDICATE BUILT ON IT.** `.` (0x2E) sorts below `/` (0x2F), so the entries
                    // that map to `child` — `child` itself and everything under `child/` — are
                    // split by `child.json`, a DIFFERENT child, sitting between them. The old
                    // predicate therefore read true-false-true over exactly the interleaving the
                    // tests in this file document, and `partition_point` is defined only on a
                    // slice the predicate PARTITIONS: on anything else it may land anywhere, and
                    // landing past the stray drops a real child from the listing.
                    //
                    // So step over the leading block only, with a predicate that is monotone on
                    // its own terms, and let the run's order deliver the stray as its own child.
                    // `child` then recurs when the `child/` block is reached; the `BTreeSet` was
                    // always there to absorb that. Cost stays O(children + strays) searches.
                    let head = ix.name(&tail[0]);
                    let step = if head.len() == child.len() {
                        // The equal run is contiguous and sits at the front of `tail`.
                        tail.partition_point(|e| ix.name(e) == child.as_str())
                    } else {
                        // Names with prefix `child/` form one contiguous range, and `tail[0]` is
                        // its first member.
                        tail.partition_point(|e| {
                            let m = ix.name(e);
                            m.len() > child.len()
                                && m.as_bytes()[child.len()] == b'/'
                                && m.starts_with(&child)
                        })
                    };
                    // `tail[0]` satisfies whichever branch was taken, so `step >= 1` and the walk
                    // always advances; the clamp is what makes that a guarantee rather than a
                    // reading of the two predicates above.
                    debug_assert!(step >= 1);
                    i += step.max(1);
                } else {
                    i += tail.partition_point(|e| {
                        let m = ix.name(e);
                        m.starts_with(&child)
                            && (m.len() == child.len() || m.as_bytes()[child.len()] == b'/')
                    });
                }
                out.insert(child);
            }
        } else {
            let plen = prefix.len();
            for e in self.list_resources(prefix) {
                if let Some(rest) = e.get(plen..) {
                    let first = rest.trim_end_matches('/').split('/').next().unwrap_or("");
                    if !first.is_empty() {
                        out.insert(format!("{prefix}{first}"));
                    }
                }
            }
            return out.into_iter().collect();
        }
        // A directory source is walked live (its tree is not immutable), but only ONE level:
        // the deep walk's extra entries all reduce to the same first segment anyway.
        for src in self.sources_r().iter() {
            if let Source::Dir(dir) = src {
                if let Ok(rd) = std::fs::read_dir(dir.join(prefix)) {
                    for e in rd.flatten() {
                        if let Some(n) = e.file_name().to_str() {
                            out.insert(format!("{prefix}{n}"));
                        }
                    }
                }
            }
        }
        out.into_iter().collect()
    }

    /// Whether any source has a resource entry at this exact path.
    pub fn has_resource(&self, path: &str) -> bool {
        // Shared all the way down: nothing here mutates, and this is one of the three questions
        // (`exists` / `size` / `newInputStream`) the datapack reload asks per file. Under the old
        // single `Mutex` it queued behind whatever inflate was in flight.
        //
        // The index answers the jar half outright — one binary search instead of one lock and
        // one hash per jar, forty deep — and the answer does not depend on WHICH jar wins, so
        // the order the sources are scanned in does not matter here.
        let g = loader_gates();
        if let Some(ix) = self.path_index(g) {
            if ix.holds(path) {
                return true;
            }
            // No directory source means the index IS the classpath, and it has already said no.
            // The alternative was a `sources` read guard and a walk of forty entries to reach
            // forty `matches!` misses — per file, for every `exists` the datapack reload asks.
            if lg_of(g, LG_LOADER3, fast_loader3_live) && !ix.has_dirs {
                return false;
            }
            return self.sources_r().iter().any(|src| match src {
                Source::Jar(_) => false,
                Source::Dir(dir) => dir.join(path).is_file(),
            });
        }
        self.sources_r().iter().any(|src| match src {
            Source::Jar(jar) => jar.read().unwrap_or_else(|e| e.into_inner()).has_entry(path),
            Source::Dir(dir) => dir.join(path).is_file(),
        })
    }

    /// Uncompressed resource size from metadata when available; does not read
    /// jar contents just to answer `Files.size/readAttributes`.
    pub fn resource_size(&self, path: &str) -> Option<u64> {
        // Classpath ORDER is observable here — the first source that answers wins — so the index
        // is used as a filter over the same walk, never as a replacement for it. A jar the index
        // says does not hold the name cannot answer, and is skipped without its lock. (A jar that
        // holds it can still decline: `entry_size` has no size for a DIRECTORY entry, and the
        // walk must then continue to the next source exactly as before.)
        let g = loader_gates();
        let ix = self.path_index(g);
        let owners = ix.as_ref().map(|i| i.exact(path));
        // ⭐**A NAME NO JAR HOLDS, ON A JARS-ONLY CLASSPATH, IS ALREADY ANSWERED.** The index
        // covers every jar, so an empty `owners` with no directory source left is a final NO —
        // and it is the DOMINANT case here. The loop below would still take the `sources` read
        // guard and step over all ~40 entries to reach it, which is O(classpath) per query for a
        // question the binary search already decided.
        if lg_of(g, LG_LOADER3, fast_loader3_live)
            && owners.is_some_and(|o| o.is_empty())
            && ix.as_ref().is_some_and(|i| !i.has_dirs)
        {
            return None;
        }
        for (i, src) in self.sources_r().iter().enumerate() {
            if let (Some(o), Source::Jar(_)) = (owners, src) {
                if !o.iter().any(|e| e.src == i as u32) {
                    continue;
                }
            }
            match src {
                Source::Jar(jar) => {
                    if let Some(size) =
                        jar.read().unwrap_or_else(|e| e.into_inner()).entry_size(path)
                    {
                        return Some(size);
                    }
                }
                Source::Dir(dir) => {
                    if let Ok(meta) = std::fs::metadata(dir.join(path)) {
                        return Some(meta.len());
                    }
                }
            }
        }
        None
    }

    /// Whether `path` denotes a directory in any source's resource tree.
    ///
    /// `Files.walk` / `Files.find` ask this once per entry they return, and the old form asked
    /// every jar — a lock plus two hashes each — for a question the index decides in one binary
    /// search (see `PathIndex::any_jar_dir`, which reproduces `Jar::is_dir` per source rather
    /// than merging them, because "holds `p` as a file" and "has children under `p`" must be
    /// judged on the same jar).
    pub fn resource_is_dir(&self, path: &str) -> bool {
        // One word for every gate this query reads — see [`loader_gates`].
        let g = loader_gates();
        if let Some(ix) = self.path_index(g) {
            let trimmed = path.trim_end_matches('/');
            let jars_say = if trimmed.is_empty() {
                // `Jar::is_dir("")` is true for every archive; the root is a directory as long
                // as there IS a jar on the path — a fact of the source list the index was built
                // from, not something to re-walk the source list for.
                if lg_of(g, LG_LOADER3, fast_loader3_live) {
                    ix.any_jar
                } else {
                    self.sources_r().iter().any(|s| matches!(s, Source::Jar(_)))
                }
            } else {
                ix.any_jar_dir(trimmed)
            };
            if jars_say {
                return true;
            }
            // `Files.walk` / `Files.find` ask this once per entry they return, and on a jars-only
            // classpath the negative answer is already final.
            if lg_of(g, LG_LOADER3, fast_loader3_live) && !ix.has_dirs {
                return false;
            }
            return self.sources_r().iter().any(|src| match src {
                Source::Jar(_) => false,
                Source::Dir(dir) => dir.join(path).is_dir(),
            });
        }
        self.sources_r().iter().any(|src| match src {
            Source::Jar(jar) => jar.read().unwrap_or_else(|e| e.into_inner()).is_dir(path),
            Source::Dir(dir) => dir.join(path).is_dir(),
        })
    }

    fn read_bytes(&self, name: &str) -> Result<Vec<u8>, LoadError> {
        // One word for every gate this query reads — see [`loader_gates`].
        let g = loader_gates();

        // ⭐**ONE ENTRY PATH FOR THE WHOLE CLASSPATH SCAN, NOT ONE PER SOURCE.**
        // `Jar::read_class` builds `{name}.class` with a `format!` AND runs
        // `replace('.', "/")` — two allocations plus a full scan of the name — and it did that
        // once for EVERY jar on the path, to produce N identical strings for one class load.
        // A boot's classpath is the bundled server jar plus every library `add_classpath` adds,
        // and this runs ~15,000 times: pure per-class one-shot work no JIT can amortise.
        //
        // Binary names reaching the loader are already slash-form (the cache is keyed on them),
        // so the normalisation is a scan that finds nothing — kept because JVMS 4.2.1 says the
        // `.` in a binary name is a package separator, and dropping it would make dir sources
        // disagree with jar sources for a name that ever did carry one.
        let mut entry = String::with_capacity(name.len() + 6);
        if name.as_bytes().contains(&b'.') {
            entry.extend(name.chars().map(|c| if c == '.' { '/' } else { c }));
        } else {
            entry.push_str(name);
        }
        entry.push_str(".class");
        // ⭐**AND ONE ENTRY PATH FOR THE WHOLE CLASSPATH, NOT ONE PROBE PER SOURCE.** The scan
        // below runs ~15,000 times a boot over a classpath that is the bundler jar, the extracted
        // server jar and every library the bundler's `URLClassLoader` registers — around forty
        // sources, each of which was asked (shared lock, name hash, set probe) to say "no". The
        // index knows which source holds `{name}.class`, so the other thirty-nine are ruled out
        // by an integer compare, and a name no jar holds — the JDK classes served natively, the
        // dominant case — rules out all forty.
        let ix = self.path_index(g);
        let owners = ix.as_ref().map(|i| i.exact(&entry));
        // ⭐**A NAME NO JAR HOLDS, ON A JARS-ONLY CLASSPATH, IS ALREADY ANSWERED.** The index
        // covers every jar, so an empty `owners` with no directory source left is a final NO —
        // and it is the DOMINANT case here. The loop below would still take the `sources` read
        // guard and step over all ~40 entries to reach it, which is O(classpath) per query for a
        // question the binary search already decided.
        if lg_of(g, LG_LOADER3, fast_loader3_live)
            && owners.is_some_and(|o| o.is_empty())
            && ix.as_ref().is_some_and(|i| !i.has_dirs)
        {
            return Err(LoadError::NotFound(std::sync::Arc::from(name)));
        }
        let fast = lg_of(g, LG_CLASSLOAD, crate::class_loader::fast_classload_live);
        for (i, src) in self.sources_r().iter().enumerate() {
            if let (Some(o), Source::Jar(_)) = (owners, src) {
                if !o.iter().any(|e| e.src == i as u32) {
                    continue;
                }
            }
            match src {
                Source::Dir(dir) => {
                    // `is_file()` then `read()` asked the filesystem the same question twice,
                    // and each is a real kernel transition. The three kinds skipped below are
                    // exactly the ones `is_file()` used to absorb: absent, and the two a
                    // DIRECTORY read reports (Windows answers a directory open with
                    // ERROR_ACCESS_DENIED, so `PermissionDenied` has to be one of them —
                    // meaning an unreadable-but-present classfile now falls through to the
                    // next source instead of raising `Io`, which is what the JVM does anyway).
                    // Every other failure is a real I/O fault and still surfaces as `Io`.
                    let path = dir.join(&entry);
                    if !fast {
                        if path.is_file() {
                            return std::fs::read(&path).map_err(|e| LoadError::Io(e.to_string()));
                        }
                        continue;
                    }
                    match std::fs::read(&path) {
                        Ok(b) => return Ok(b),
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::NotFound
                                    | std::io::ErrorKind::PermissionDenied
                                    | std::io::ErrorKind::InvalidInput
                            ) => {}
                        Err(e) => return Err(LoadError::Io(e.to_string())),
                    }
                }
                Source::Jar(jar) => {
                    // Name index first (shared), archive second (exclusive) — see `read_resource`.
                    // Skipped when the classpath index already named this jar as the owner.
                    if owners.is_none()
                        && fast
                        && !jar.read().unwrap_or_else(|e| e.into_inner()).has_entry(&entry)
                    {
                        continue;
                    }
                    let read = jar
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .read_entry(&entry);
                    match read {
                        Ok(Some(b)) => return Ok(b),
                        Ok(None) => {}
                        Err(e) => return Err(LoadError::Io(e.to_string())),
                    }
                }
            }
        }
        Err(LoadError::NotFound(std::sync::Arc::from(name)))
    }

    /// [`Self::read_bytes`]'s dry-run twin: the SAME classpath scan in the SAME source order,
    /// with the path index DISABLED (`None` instead of `path_index(g)`), because a cold
    /// build would publish `path_index` — the one shared mutation `read_bytes` can make, and
    /// one the dry-run contract forbids. This intentionally drops the owner-index prefilter:
    /// a missing class may scan every configured source, an opt-in diagnostic-only cost.
    /// Nothing here writes any loader field.
    fn read_bytes_uncached(&self, name: &str) -> Result<Vec<u8>, LoadError> {
        let g = loader_gates();
        let mut entry = String::with_capacity(name.len() + 6);
        if name.as_bytes().contains(&b'.') {
            entry.extend(name.chars().map(|c| if c == '.' { '/' } else { c }));
        } else {
            entry.push_str(name);
        }
        entry.push_str(".class");
        let fast = lg_of(g, LG_CLASSLOAD, crate::class_loader::fast_classload_live);
        for src in self.sources_r().iter() {
            match src {
                Source::Dir(dir) => {
                    let path = dir.join(&entry);
                    if !fast {
                        if path.is_file() {
                            return std::fs::read(&path).map_err(|e| LoadError::Io(e.to_string()));
                        }
                        continue;
                    }
                    match std::fs::read(&path) {
                        Ok(b) => return Ok(b),
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::NotFound
                                    | std::io::ErrorKind::PermissionDenied
                                    | std::io::ErrorKind::InvalidInput
                            ) => {}
                        Err(e) => return Err(LoadError::Io(e.to_string())),
                    }
                }
                Source::Jar(jar) => {
                    if fast && !jar.read().unwrap_or_else(|e| e.into_inner()).has_entry(&entry) {
                        continue;
                    }
                    let read = jar
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .read_entry(&entry);
                    match read {
                        Ok(Some(b)) => return Ok(b),
                        Ok(None) => {}
                        Err(e) => return Err(LoadError::Io(e.to_string())),
                    }
                }
            }
        }
        Err(LoadError::NotFound(std::sync::Arc::from(name)))
    }
}

impl Default for ClassLoader {
    fn default() -> Self {
        Self::new()
    }
}

/// The classpath index decides `exists` / `size` / `isDirectory` / `list` for the whole boot, and
/// its two binary searches straddle a boundary that is easy to get wrong: `.` (0x2E) sorts BELOW
/// `/` (0x2F), so `data/foo`, `data/foo.json` and `data/foo/x` interleave in exactly the order
/// that breaks a naive "children are contiguous after the parent" assumption. These lock the
/// answers against `Jar`'s own definitions without needing a real archive on disk.
#[cfg(test)]
mod path_index_tests {
    use super::{loader_gates, prefix_key, PathEnt, PathIndex};

    /// Build an index over `(name, source)` pairs, sorted exactly as `PathIndex::build` sorts.
    ///
    /// Deliberately sorted by the STRING comparator, not the key-first one: these tests then
    /// exercise the key-shortcut queries against an array ordered the old way, which is the
    /// claim `PathEnt::key` makes (the two comparators are the same total order).
    fn ix(names: &[(&str, u32)]) -> PathIndex {
        let mut blob = String::new();
        let mut ents: Vec<PathEnt> = Vec::new();
        for (n, s) in names {
            ents.push(PathEnt { key: prefix_key(n), off: blob.len(), len: n.len() as u32, src: *s });
            blob.push_str(n);
        }
        let b = &blob;
        ents.sort_unstable_by(|x, y| {
            b[x.off..x.off + x.len as usize]
                .cmp(&b[y.off..y.off + y.len as usize])
                .then(x.src.cmp(&y.src))
        });
        PathIndex { epoch: 0, blob, ents: ents.into_boxed_slice(), has_dirs: false, any_jar: true, gates: loader_gates() }
    }

    #[test]
    fn exact_returns_every_owner_in_classpath_order() {
        let i = ix(&[("a/B.class", 2), ("z", 1), ("a/B.class", 0)]);
        let owners: Vec<u32> = i.exact("a/B.class").iter().map(|e| e.src).collect();
        assert_eq!(owners, vec![0, 2]);
        assert!(i.exact("a/C.class").is_empty());
    }

    #[test]
    fn under_is_the_prefix_run_and_nothing_else() {
        let i = ix(&[("a/b", 0), ("a/c", 0), ("b/x", 0), ("a", 0)]);
        let got: Vec<&str> = i.under("a/").iter().map(|e| i.name(e)).collect();
        assert_eq!(got, vec!["a/b", "a/c"]);
        // Prefix, not path segment — same as `Jar::entries_under`, which `list_resources` mirrors.
        let got: Vec<&str> = i.under("a").iter().map(|e| i.name(e)).collect();
        assert_eq!(got, vec!["a", "a/b", "a/c"]);
    }

    #[test]
    fn dir_is_children_minus_own_entry_per_source() {
        // `data/foo.json` sorts BETWEEN `data/foo` and `data/foo/x`; the child search must not
        // stop there, and must not mistake it for a child of `data/foo`.
        let i = ix(&[("data/foo.json", 0), ("data/foo/x", 0), ("data/bar", 0)]);
        assert!(i.any_jar_dir("data/foo"));
        assert!(!i.any_jar_dir("data/foo.json"));
        assert!(!i.any_jar_dir("data/bar"));
        assert!(i.any_jar_dir("data"));

        // One jar holding `data/foo` as a FILE does not call it a directory (`Jar::is_dir`)...
        let i = ix(&[("data/foo", 0), ("data/foo/x", 0)]);
        assert!(!i.any_jar_dir("data/foo"));
        // ...but another jar that only has children still does, which is why the two halves are
        // judged per source rather than merged.
        let i = ix(&[("data/foo", 0), ("data/foo/x", 1)]);
        assert!(i.any_jar_dir("data/foo"));
    }

    /// `holds` replaces `!exact(..).is_empty()` on the `has_resource` path, and the two must
    /// agree on the boundary cases the sort puts next to each other.
    #[test]
    fn holds_agrees_with_the_run_it_replaced() {
        let i = ix(&[("a/B.class", 2), ("a/B.classx", 0), ("z", 1), ("a/B.class", 0)]);
        for probe in ["a/B.class", "a/B.classx", "z", "a/B.clas", "a", "", "zz"] {
            assert_eq!(i.holds(probe), !i.exact(probe).is_empty(), "probe {probe:?}");
        }
    }

    /// The binary-searched upper bound must delimit exactly the run the forward walk did — and
    /// the interesting inputs are the ones that sort ADJACENT to the prefix without sharing it.
    #[test]
    fn run_ends_where_the_forward_walk_ended() {
        let i = ix(&[("a", 0), ("a/b", 0), ("a/b/c", 0), ("a/c", 0), ("a0", 0), ("b", 0)]);
        for probe in ["a", "a/", "a/b", "a/b/", "a0", "b", "c", "", "a/b/c/"] {
            let want: Vec<&str> = i
                .ents
                .iter()
                .filter(|e| i.name(e).starts_with(probe))
                .map(|e| i.name(e))
                .collect();
            let got: Vec<&str> = i.under(probe).iter().map(|e| i.name(e)).collect();
            assert_eq!(got, want, "prefix {probe:?}");
        }
    }

    /// ⭐**THE ONE CLAIM THE INLINE KEY MAKES.** `PathEnt::key` is allowed to decide a comparison
    /// only when the two keys DIFFER, and the whole index is correct iff that verdict is the
    /// string verdict. Exhaustive over the boundary shapes: names shorter/longer than the 8-byte
    /// key, names that are prefixes of each other, names differing at every byte position 0..8
    /// and beyond it, the empty name, and a name carrying a NUL (which zip does not forbid and
    /// which is the one input a zero-padded key could plausibly confuse).
    #[test]
    fn key_order_never_disagrees_with_string_order() {
        use std::cmp::Ordering;
        let names = [
            "", "a", "a\u{0}", "a\u{0}b", "ab", "abcdefg", "abcdefgh", "abcdefgh0", "abcdefghi",
            "abcdefg/", "abcdefg.", "b", "data", "data/", "data.json", "data/foo", "data/foo.json",
            "data/foo/x", "net/minecraft/server/Main.class", "net/minecraft/server/Maio.class",
            "\u{7f}", "\u{0}",
        ];
        for x in names {
            for y in names {
                let (kx, ky) = (prefix_key(x), prefix_key(y));
                let full = x.cmp(y);
                let ko = kx.cmp(&ky);
                // A differing key must reproduce the string verdict exactly. An equal key is an
                // ABSTENTION, never a claim of equality — every caller falls through to the
                // string compare there, so any verdict is allowed.
                if ko != Ordering::Equal {
                    assert_eq!(ko, full, "keys decided {x:?} vs {y:?} the wrong way");
                }
                // Equal names must never produce different keys, or the fall-through is skipped.
                if full == Ordering::Equal {
                    assert_eq!(kx, ky, "equal names {x:?} disagreed on key");
                }
            }
        }
        // `dir_prefix_key(d)` must be the key of the virtual name `d + "/"`, which is what
        // `any_jar_dir` binary-searches for.
        for d in ["", "a", "data", "data/foo", "abcdefg", "abcdefgh", "abcdefghij"] {
            assert_eq!(
                super::dir_prefix_key(d),
                prefix_key(&format!("{d}/")),
                "virtual dir key for {d:?}"
            );
        }
    }

    #[test]
    fn dir_key_orders_against_the_virtual_slash() {
        use std::cmp::Ordering;
        assert_eq!(PathIndex::cmp_dir_key("dat", "data"), Ordering::Less);
        assert_eq!(PathIndex::cmp_dir_key("data", "data"), Ordering::Less);
        assert_eq!(PathIndex::cmp_dir_key("data.json", "data"), Ordering::Less);
        assert_eq!(PathIndex::cmp_dir_key("data/", "data"), Ordering::Equal);
        assert_eq!(PathIndex::cmp_dir_key("data/x", "data"), Ordering::Greater);
        assert_eq!(PathIndex::cmp_dir_key("datb", "data"), Ordering::Greater);
    }
}

/// The netty event-executor family question (`is_netty_executor_family`) is a pure
/// `type_closure` membership read, so its boundary cases are pinnable from synthetic
/// classfiles alone — no jars: the root itself, a subclass edge, an interface edge, a
/// non-member, and the cross-loader identity a name-keyed global could not give.
#[cfg(test)]
mod netty_executor_family_tests {
    use crate::class_loader::ClassLoader;
    use rava_classfile::{ClassFile, Constant, ConstantPool, MemberInfo};

    const ROOT: &str = crate::net::EVENT_EXECUTOR_GROUP_ROOT;

    fn define(loader: &ClassLoader, name: &str, superc: Option<&str>, ifaces: &[&str]) {
        let file = ClassFile::synthesize(
            ConstantPool::from_entries(vec![Constant::Unusable]),
            0x0021,
            name.to_string(),
            superc.map(str::to_string),
            ifaces.iter().map(|s| s.to_string()).collect(),
            Vec::<MemberInfo>::new(),
        );
        loader.define_synthetic(file);
    }

    #[test]
    fn loaded_class_carries_its_native_family_fact() {
        let loader = ClassLoader::new();
        define(&loader, "java/lang/Integer", None, &[]);

        assert_eq!(
            loader.cached_native_family("java/lang/Integer"),
            Some(crate::owner_bits::NativeFamilyFact::for_owner_name(
                "java/lang/Integer",
            )),
        );
        assert_eq!(loader.cached_native_family("Synthetic/NotLoaded"), None);
    }

    #[test]
    fn root_is_family_with_nothing_loaded() {
        // The closure seeds {start}: the root is a member of itself before anything
        // is loaded — the family question never needs the root's own classfile.
        assert!(ClassLoader::new().is_netty_executor_family(ROOT));
    }

    #[test]
    fn synthetic_subclass_and_interface_closure_are_family() {
        let l = ClassLoader::new();
        define(&l, "Syn/NettySub", Some(ROOT), &[]);
        assert!(l.is_netty_executor_family("Syn/NettySub"), "subclass edge reaches the root");
        define(&l, "Syn/NettyIfaceImpl", None, &[ROOT]);
        assert!(l.is_netty_executor_family("Syn/NettyIfaceImpl"), "interface edge reaches the root");
    }

    #[test]
    fn object_and_unrelated_are_not_family() {
        let l = ClassLoader::new();
        // An unloadable terminal name records itself and stops: not a member.
        assert!(!l.is_netty_executor_family("java/lang/Object"));
        define(&l, "Syn/Plain", Some("java/lang/Object"), &[]);
        assert!(!l.is_netty_executor_family("Syn/Plain"));
    }

    #[test]
    fn no_cross_loader_leak() {
        let a = ClassLoader::new();
        define(&a, "Syn/LeakSub", Some(ROOT), &[]);
        assert!(a.is_netty_executor_family("Syn/LeakSub"));
        // A fresh loader on the SAME thread re-derives from its own (empty) state —
        // the answer loader A derived must not leak by name.
        let b = ClassLoader::new();
        assert!(!b.is_netty_executor_family("Syn/LeakSub"), "name-keyed global would have leaked");
    }
}


/// The dry-run loading scope: staged visibility, no shared mutation, and cleanup. Classfiles
/// are written to a real temporary directory source (the same `Dir` source `read_bytes` scans),
/// so the dry parse exercises the true classpath order and the negative case is a name no
/// source supplies.
#[cfg(test)]
mod dry_run_load_tests {
    use super::*;
    use rava_classfile::{Constant, ConstantPool, MemberInfo};

    /// A minimal but real classfile for `name` with one Utf8 (its name) and one Class entry,
    /// no super, no members. `ClassFile::parse` resolves `this_class` from it; everything the
    /// loader's hierarchy walks read (super, interfaces, access flags) is exercised through
    /// the bytes below.
    fn class_bytes(name: &str) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes()); // minor
        b.extend_from_slice(&52u16.to_be_bytes()); // major
        b.extend_from_slice(&3u16.to_be_bytes()); // constant_pool_count: slots 1..2 usable
        // 1: Utf8 <name>
        b.push(1);
        b.extend_from_slice(&(name.len() as u16).to_be_bytes());
        b.extend_from_slice(name.as_bytes());
        // 2: Class -> 1
        b.push(7);
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&0x0021u16.to_be_bytes()); // access_flags: public super
        b.extend_from_slice(&2u16.to_be_bytes()); // this_class
        b.extend_from_slice(&0u16.to_be_bytes()); // super_class: none
        b.extend_from_slice(&0u16.to_be_bytes()); // interfaces_count
        b.extend_from_slice(&0u16.to_be_bytes()); // fields_count
        b.extend_from_slice(&0u16.to_be_bytes()); // methods_count
        b.extend_from_slice(&0u16.to_be_bytes()); // class attributes_count
        b
    }

    /// A loader whose classpath is one directory source holding the given classfiles. The
    /// directory is created under `std::env::temp_dir()` with a unique name and removed after
    /// each test (no third-party temp-dir dependency is available to this crate's tests).
    fn loader_with(files: &[(&str, &str)]) -> (ClassLoader, std::path::PathBuf) {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rava-dry-run-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        for (file, name) in files {
            let dest = dir.join(file);
            // `file` carries subdirectories (`Dry/Plain.class`): the destination's parent must
            // exist before the write.
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).expect("create classfile parent dir");
            }
            std::fs::write(dest, class_bytes(name)).expect("write classfile");
        }
        (ClassLoader::new().with_dir(dir.clone()), dir)
    }

    /// Best-effort removal of a test's temp directory.
    fn rmdir(dir: &std::path::Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    fn epoch_and_counts(l: &ClassLoader) -> (u64, usize) {
        (l.epoch(), l.loaded_count())
    }

    /// THE contract: staged lookups work repeatedly inside the scope, disappear after it, and
    /// no shared state (cache, missing, super-chain memo, type-closure memo, sub-index, epoch)
    /// moved.
    #[test]
    fn staged_lookups_work_then_disappear_without_shared_mutation() {
        let (l, dir) = loader_with(&[("Dry/Plain.class", "Dry/Plain")]);
        let (epoch0, count0) = epoch_and_counts(&l);
        assert!(!ClassLoader::dry_run_loads_active());
        assert_eq!(l.dry_run_staged_count(), 0);

        let got = ClassLoader::with_dry_run_loads(|| {
            assert!(ClassLoader::dry_run_loads_active());
            // First ask: a MISS parsed to the stage.
            let a = l.get_or_load("Dry/Plain").expect("staged parse");
            // The stage is keyed per loader: the count is for THIS loader.
            assert_eq!(l.dry_run_staged_count(), 1);
            assert_eq!(l.dry_run_staged_names(), vec!["Dry/Plain".to_string()]);
            assert!(l.loaded_class_names().contains(&"Dry/Plain".to_string()));
            // Second ask: served from the stage, the same Arc, no re-parse.
            let b = l.get_or_load("Dry/Plain").expect("staged hit");
            assert!(Arc::ptr_eq(&a, &b));
            assert_eq!(l.dry_run_staged_count(), 1, "repeat lookup must not restage");
            // The hierarchy helpers see the staged class...
            let chain = l.super_chain("Dry/Plain");
            assert_eq!(chain.first().unwrap().as_ref(), "Dry/Plain");
            let closure = l.type_closure("Dry/Plain");
            assert!(closure.contains("Dry/Plain".into()));
            // ...without any memo being filled by the dry walks.
            assert!(l.super_chain_cache.read().unwrap_or_else(|e| e.into_inner()).is_empty());
            assert!(l.type_closure_cache.read().unwrap_or_else(|e| e.into_inner()).is_empty());
            a
        });

        assert_eq!(got.file.this_class, "Dry/Plain");
        // The scope is gone, and with it the stage and every staged answer.
        assert!(!ClassLoader::dry_run_loads_active());
        assert_eq!(l.dry_run_staged_count(), 0);
        assert!(l.dry_run_staged_names().is_empty());
        assert!(!l.loaded_class_names().contains(&"Dry/Plain".to_string()));
        // DIRECT read-only proof of no leakage into the real caches.
        assert!(l.cached("Dry/Plain").is_none(), "staged class leaked into the real cache");
        assert!(!l.known_missing("Dry/Plain"), "staged miss leaked into the missing set");
        // No shared state moved at all.
        assert_eq!(epoch_and_counts(&l), (epoch0, count0), "epoch or loaded_count changed");
        assert!(l.super_chain_cache.read().unwrap_or_else(|e| e.into_inner()).is_empty());
        assert!(l.type_closure_cache.read().unwrap_or_else(|e| e.into_inner()).is_empty());
        assert!(!l.sub_index.read().unwrap_or_else(|e| e.into_inner()).0, "sub-index built");
        rmdir(&dir);
    }

    /// Already-published classes stay readable inside the scope, and a staged class and a
    /// published one coexist in `loaded_class_names`.
    #[test]
    fn published_classes_stay_readable_and_coexist_with_stage() {
        let (l, dir) = loader_with(&[("Dry/Staged.class", "Dry/Staged")]);
        l.define_synthetic(
            ClassFile::synthesize(
                ConstantPool::from_entries(vec![Constant::Unusable]),
                0x0021,
                "Dry/Published".to_string(),
                None,
                Vec::<String>::new(),
                Vec::<MemberInfo>::new(),
            ),
        );
        let (epoch0, count0) = epoch_and_counts(&l);
        ClassLoader::with_dry_run_loads(|| {
            let pub_ = l.get_or_load("Dry/Published").expect("published");
            let stage = l.get_or_load("Dry/Staged").expect("staged");
            assert_ne!(Arc::as_ptr(&pub_), Arc::as_ptr(&stage));
            let names = l.loaded_class_names();
            assert!(names.contains(&"Dry/Published".to_string()));
            assert!(names.contains(&"Dry/Staged".to_string()));
            // The published half also still answers concrete_subtypes through the real index.
            let subs = l.concrete_subtypes("Dry/Published", 8).expect("under cap");
            assert!(subs.iter().any(|c| c.file.this_class == "Dry/Published"));
        });
        assert_eq!(epoch_and_counts(&l), (epoch0, count0));
        assert_eq!(l.loaded_count(), count0);
        rmdir(&dir);
    }

    /// A staged NEGATIVE answer (no source holds the name) records nothing in the shared
    /// missing set — during the scope or after it.
    #[test]
    fn staged_miss_records_nothing_in_missing() {
        let (l, dir) = loader_with(&[]);
        let (epoch0, count0) = epoch_and_counts(&l);
        ClassLoader::with_dry_run_loads(|| {
            let e = l.get_or_load("Dry/Nowhere").err().expect("dry miss must fail");
            assert!(matches!(e, LoadError::NotFound(_)), "got {e:?}");
            assert!(!l.known_missing("Dry/Nowhere"), "dry miss wrote the missing set");
        });
        assert!(!l.known_missing("Dry/Nowhere"));
        assert_eq!(epoch_and_counts(&l), (epoch0, count0));
        rmdir(&dir);
    }

    /// A gil-free worker inside a dry scope still fails closed: no parse, no stage, no answer.
    #[test]
    fn dry_scope_refuses_to_load_on_a_gilfree_worker() {
        let _serial = crate::parallel::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (l, dir) = loader_with(&[("Dry/Worker.class", "Dry/Worker")]);
        crate::parallel::clear_abort();
        crate::parallel::set_gilfree(true);
        let res = ClassLoader::with_dry_run_loads(|| l.get_or_load("Dry/Worker"));
        crate::parallel::set_gilfree(false);
        crate::parallel::clear_abort();
        match res {
            Err(LoadError::WorkerRefused(_)) => {}
            Err(e) => panic!("worker must refuse rather than report a load error: {e:?}"),
            Ok(_) => panic!("worker must fail closed before staging"),
        }
        assert_eq!(l.dry_run_staged_count(), 0, "worker staged a class");
        assert!(l.cached("Dry/Worker").is_none());
        rmdir(&dir);
    }

    /// Nested scopes: the inner one sees the outer's staged classes, and the outer's stage
    /// survives the inner's close.
    #[test]
    fn nested_scopes_share_the_stage_and_outer_survives_inner() {
        let (l, dir) = loader_with(&[("Dry/Outer.class", "Dry/Outer"), ("Dry/Inner.class", "Dry/Inner")]);
        let (epoch0, count0) = epoch_and_counts(&l);
        ClassLoader::with_dry_run_loads(|| {
            l.get_or_load("Dry/Outer").expect("outer stage");
            ClassLoader::with_dry_run_loads(|| {
                // The inner ask finds the OUTER scope's entry (one shared thread-local stage).
                let again = l.get_or_load("Dry/Outer").expect("outer stage visible inside");
                let inner = l.get_or_load("Dry/Inner").expect("inner stage");
                assert!(Arc::ptr_eq(&again, &l.get_or_load("Dry/Outer").unwrap()));
                assert_eq!(l.dry_run_staged_count(), 2, "one shared stage, not two");
                drop(inner);
            });
            // The outer scope is still open and its stage intact.
            assert!(ClassLoader::dry_run_loads_active());
            assert_eq!(l.dry_run_staged_count(), 2);
        });
        assert!(!ClassLoader::dry_run_loads_active());
        assert_eq!(epoch_and_counts(&l), (epoch0, count0));
        rmdir(&dir);
    }

    /// Two loaders in one scope keep separate stages: a name staged for loader A does not
    /// answer loader B.
    #[test]
    fn stages_are_keyed_per_loader() {
        let (a, da) = loader_with(&[("Dry/OnlyA.class", "Dry/OnlyA")]);
        let (b, db) = loader_with(&[]);
        ClassLoader::with_dry_run_loads(|| {
            a.get_or_load("Dry/OnlyA").expect("staged for A");
            assert_eq!(a.dry_run_staged_count(), 1);
            assert_eq!(b.dry_run_staged_count(), 0);
            assert!(b.get_or_load("Dry/OnlyA").is_err(), "B saw A's stage");
        });
        assert_eq!(a.dry_run_staged_count(), 0);
        rmdir(&da);
        rmdir(&db);
    }

    /// The path index is untouched by dry loads even on a loader that never built one, and a
    /// pre-existing index is not rebuilt or invalidated (sources_epoch never moves).
    #[test]
    fn dry_loads_touch_no_path_index_state() {
        let (l, dir) = loader_with(&[("Dry/Pi.class", "Dry/Pi")]);
        let sources_epoch0 = l.sources_epoch.load(std::sync::atomic::Ordering::Acquire);
        let built0 = l.path_index.read().unwrap_or_else(|e| e.into_inner()).is_some();
        ClassLoader::with_dry_run_loads(|| {
            let _ = l.get_or_load("Dry/Pi");
            let _ = l.get_or_load("Dry/Nowhere");
        });
        let sources_epoch1 = l.sources_epoch.load(std::sync::atomic::Ordering::Acquire);
        let built1 = l.path_index.read().unwrap_or_else(|e| e.into_inner()).is_some();
        assert_eq!(sources_epoch0, sources_epoch1);
        assert_eq!(built0, built1);
        rmdir(&dir);
    }
}
