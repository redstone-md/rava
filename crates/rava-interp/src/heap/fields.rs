//! Field access: encoding, decoding, hints, and the pinned/mirror paths.

use crate::heap::*;

// ─── THE HEAP GATE WORD ────────────────────────────────────────────────────────
//
// ⭐**ONE RELAXED WORD FOR EVERY PROCESS-CONSTANT GATE ON A FIELD OR ELEMENT ACCESS.**
//
// Field and element access are the hottest host functions in this engine after dispatch, and
// every one of them ended in a run of independent `OnceLock<bool>` reads that exist only to
// learn that a default-OFF migration flag is still off. A `OnceLock::get_or_init` is not a free
// `mov`: it is an ACQUIRE load of the once-state, a branch, and then a SECOND load to deref the
// `&bool` it returns — and the acquire is an OPTIMISATION BARRIER, so LLVM may neither hoist it
// out of the caller's loop nor forward one read across the opaque calls that sit between two of
// them. N statics therefore cost N acquire pairs per operation, not one.
//
// Counted on the product path (`--features next-gen-gc`), per operation, BEFORE this word:
//
//   put_field_hint  (interp + compiled putfield funnel) 2 — pinned_mirror_enabled, shadow_enabled
//   put_field_by_id (by-name / by-`Syn` writes)         2 — the same two
//   get_field       (by-name reads)                     2 — pinned_verify_enabled, pinned_read_enabled
//   get_field_hint  (interp getfield funnel)            1 — pinned_read_enabled, reached through a
//                                                          NON-INLINED call to `pinned_read_field`
//   Heap::array_set (aastore/iastore funnel)            1 — m3biome::enabled
//   Heap::put_array (8.74 M array births a boot)        1 — m3biome::enabled (via `mark_arr_gen`)
//   alloc_array / alloc_values / alloc_ref_array_by_id  2 — m3heap::probe_on is `enabled() ||
//                                                          worldgen_probe()`, i.e. TWO OnceLocks
//   get_field_by_id (by-id + every `Syn` read)          2 — diag::field_read (a `OnceLock<Meter>`,
//                                                          whose `MeterGuard` also left live DROP
//                                                          GLUE in the function) and
//                                                          diag::field_scan_len (a
//                                                          `OnceLock<Histogram>`)
//
// AFTER: each of those paths reads THIS word once at its natural entry and bit-tests a value
// already in a register. The store is two bytes that are permanently L1-resident, the load is
// RELAXED (a plain `movzx` on x86 that LLVM may CSE and hoist), and the branches predict
// perfectly because every bit is a constant of the process.
//
// ⛔**NOT A `OnceLock` ITSELF, DELIBERATELY** — that would re-introduce exactly the acquire this
// word exists to delete. Same shape and same reasoning as `heap::arraytypes::fast_arrays`, whose
// comment records the ledger entry (one relaxed atomic re-introduced on the operand-stack push
// path measured +17%): a relaxed `AtomicU16` on x86 is a plain load from a static. Table size:
// **2 bytes** — this round is removal, not a side table.
//
// ⚠**POSITION IS PRESERVED.** Every folded gate was already the FIRST statement of a pure
// early-return guard (`pinned_mirror_store`, `pinned_read_field`) or the head of a diagnostic
// block with no side effect of its own. Nothing here decides which of two semantic paths runs,
// and no side-effecting gate changed order. `class_loader::any_override()` is deliberately NOT
// in this word: it is a MUTABLE atomic flipped at class-load time, not a constant of the process.
//
// Kill switch: `RAVA_NO_HEAP_GATEWORD=1` (or `RAVA_NO_EMIT_MORE`) sets `HG_LEGACY`, and every
// accessor below then reads its own original static at its own call site, exactly as before. The
// word is still built — it has to be, to carry that bit — so the switch restores the READS,
// which is what is being measured, not the answers, which never differed.
static HEAP_GATES: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

/// Always set once the environment has been read, so a word of `0` means "not computed yet" and
/// the byte needs no separate initialised flag (and no sentinel value outside the bit space).
const HG_READY: u16 = 1 << 0;
/// `nextgen::pinned_mirror_enabled()` — B3 dual-store mirror writes. Default OFF.
#[cfg_attr(not(feature = "next-gen-gc"), allow(dead_code))]
const HG_MIRROR: u16 = 1 << 1;
/// `nextgen::pinned_read_enabled()` — B4 read-flip. Default OFF.
#[cfg_attr(not(feature = "next-gen-gc"), allow(dead_code))]
const HG_PREAD: u16 = 1 << 2;
/// `nextgen::pinned_verify_enabled()` — B4 assert-only cross-check. Default OFF.
#[cfg_attr(not(feature = "next-gen-gc"), allow(dead_code))]
const HG_VERIFY: u16 = 1 << 3;
/// `nextgen::shadow_enabled()` — the dry-run card barrier. Default OFF.
#[cfg_attr(not(feature = "next-gen-gc"), allow(dead_code))]
const HG_SHADOW: u16 = 1 << 4;
/// `m3biome::enabled()` — the array write/birth shape probe. Default OFF.
#[cfg_attr(not(feature = "next-gen-gc"), allow(dead_code))]
const HG_BIOME: u16 = 1 << 5;
/// `m3heap::probe_on()` — the allocation classifier, itself TWO OnceLocks. Default OFF.
const HG_M3HEAP: u16 = 1 << 6;
/// `diag::any_armed()` — is ANY instrument selected? Default OFF (`RAVA_DIAG` unset). Gating the
/// whole instrument block of `get_field_by_id` on this is observationally identical for the same
/// reason `put_array`'s block already argues: `dump` returns early when nothing is armed, and no
/// counter can be armed while the selector is empty.
const HG_DIAG: u16 = 1 << 7;
/// `RAVA_NO_HEAP_GATEWORD` / `RAVA_NO_EMIT_MORE`: re-read every gate at its own call site.
const HG_LEGACY: u16 = 1 << 8;
/// `RAVA_NO_FAST_FIELDDERIVE` / `RAVA_NO_EMIT_MORE`: SET means "re-derive the field block's
/// sections once per section, as before" — i.e. the bit is the KILL, so the fast shape is the
/// default and the switch restores the old code verbatim. See `BlobView` below.
const HG_NODERIVE: u16 = 1 << 9;

/// The word. One relaxed byte load on every path that used to take between one and two
/// acquire-ordered `OnceLock` pairs.
#[inline(always)]
pub(crate) fn heap_gates() -> u16 {
    let w = HEAP_GATES.load(std::sync::atomic::Ordering::Relaxed);
    if w != 0 {
        return w;
    }
    heap_gates_init()
}

/// Read the environment once, off the hot path. Every bit is the SAME predicate read through the
/// SAME accessor, so an armed run behaves exactly as it did.
#[cold]
#[inline(never)]
fn heap_gates_init() -> u16 {
    let mut w = HG_READY;
    #[cfg(feature = "next-gen-gc")]
    {
        if crate::nextgen::pinned_mirror_enabled() {
            w |= HG_MIRROR;
        }
        if crate::nextgen::pinned_read_enabled() {
            w |= HG_PREAD;
        }
        if crate::nextgen::pinned_verify_enabled() {
            w |= HG_VERIFY;
        }
        if crate::nextgen::shadow_enabled() {
            w |= HG_SHADOW;
        }
        if crate::m3biome::enabled() {
            w |= HG_BIOME;
        }
    }
    if crate::m3heap::probe_on() {
        w |= HG_M3HEAP;
    }
    if crate::diag::any_armed() {
        w |= HG_DIAG;
    }
    // Falls together with the other emit-more switches, as every gate in this tree does, so the
    // whole round bisects in a single run.
    if std::env::var_os("RAVA_NO_HEAP_GATEWORD").is_some() || crate::flags::emit_more_disabled() {
        w |= HG_LEGACY;
    }
    // The one-derivation field-block view. Carried in THIS word and not in a `OnceLock` of its
    // own for exactly the reason the word exists: an acquire load on the getfield funnel is the
    // cost this file spent a round deleting, and re-adding one to gate the deletion of another
    // would be self-cancelling. `HG_LEGACY` does not apply — there is no prior static to
    // re-read; this bit IS the answer.
    // MEASURED OPT-IN (round 31): this block is neutral on its own (median +0.60%) and it
    // CANCELS the callpath+crossing2 win (pair alone +3.77%/+6.02% when disabled, pair+five
    // +0.31%). Default is therefore the historic re-derive; `RAVA_FAST_FIELDDERIVE=1` arms it.
    if std::env::var_os("RAVA_FAST_FIELDDERIVE").is_none() || crate::flags::emit_more_disabled()
    {
        w |= HG_NODERIVE;
    }
    HEAP_GATES.store(w, std::sync::atomic::Ordering::Relaxed);
    w
}

/// B3 mirror writes armed? (`w` = one `heap_gates()` read the caller already made.)
#[cfg(feature = "next-gen-gc")]
#[inline(always)]
pub(crate) fn hg_mirror(w: u16) -> bool {
    if w & HG_LEGACY != 0 {
        return crate::nextgen::pinned_mirror_enabled();
    }
    w & HG_MIRROR != 0
}

/// B4 read-flip armed?
#[cfg(feature = "next-gen-gc")]
#[inline(always)]
pub(crate) fn hg_pread(w: u16) -> bool {
    if w & HG_LEGACY != 0 {
        return crate::nextgen::pinned_read_enabled();
    }
    w & HG_PREAD != 0
}

/// B4 mirror-vs-blob cross-check armed?
#[cfg(feature = "next-gen-gc")]
#[inline(always)]
pub(crate) fn hg_verify(w: u16) -> bool {
    if w & HG_LEGACY != 0 {
        return crate::nextgen::pinned_verify_enabled();
    }
    w & HG_VERIFY != 0
}

/// Shadow card barrier armed?
#[cfg(feature = "next-gen-gc")]
#[inline(always)]
pub(crate) fn hg_shadow(w: u16) -> bool {
    if w & HG_LEGACY != 0 {
        return crate::nextgen::shadow_enabled();
    }
    w & HG_SHADOW != 0
}

/// M3 biome shape probe armed?
#[cfg(feature = "next-gen-gc")]
#[inline(always)]
pub(crate) fn hg_biome(w: u16) -> bool {
    if w & HG_LEGACY != 0 {
        return crate::m3biome::enabled();
    }
    w & HG_BIOME != 0
}

/// Any `RAVA_DIAG` instrument selected at all?
#[inline(always)]
pub(crate) fn hg_diag(w: u16) -> bool {
    if w & HG_LEGACY != 0 {
        return crate::diag::any_armed();
    }
    w & HG_DIAG != 0
}

/// M3 allocation classifier armed? Folds `m3heap::probe_on()`'s TWO `OnceLock`s into one bit.
#[inline(always)]
pub(crate) fn hg_m3heap(w: u16) -> bool {
    if w & HG_LEGACY != 0 {
        return crate::m3heap::probe_on();
    }
    w & HG_M3HEAP != 0
}

/// Derive the field block ONCE per access? (Default yes; `RAVA_NO_FAST_FIELDDERIVE` says no.)
#[inline(always)]
pub(crate) fn hg_derive(w: u16) -> bool {
    w & HG_NODERIVE == 0
}

// ─── ONE DERIVATION OF THE FIELD BLOCK PER ACCESS ──────────────────────────────
//
// ⭐**A SINGLE FIELD ACCESS DERIVED THE SAME FOUR FACTS BETWEEN THREE AND SEVEN TIMES.**
//
// A `FieldBlob` is ONE allocation whose header is `[count:u32 @0][cap:u32 @4]` and whose three
// sections are at `cells @8`, `ids @8+8·cap`, `kinds @8+12·cap`. So the facts an access needs
// are exactly four, and every one of them is a constant of the block for the whole access:
//
//   the slot COUNT `n`   — the blob header word at +0, and the bound of ALL THREE sections
//   the CAPACITY `cap`   — the header word at +4, the only input to the ids/kinds offsets
//   the three SECTION base pointers, each `base + a const or a const·cap`
//
// `ids()`, `cells()` and `kinds()` each re-derive their own share from scratch: `ids()` loads
// `len` and `cap`, `cells()` loads `len`, `kinds()` loads `len` and `cap` — and each then builds
// a `&[_]` whose every index pays its OWN bounds test against the very `n` the caller has
// already tested. Counted on the product path, per access, BEFORE this view:
//
//   get_field_hint  HIT  (every interpreted getfield)   len ×3, cap ×2, `i < n` tested ×4
//                        — one explicit `hi < ids.len()` and then a bounds test on each of
//                          `ids[hi]`, `cells[hi]`, `kinds[hi]`, three of them with a panic pad
//   get_field_hint  MISS (a cold site on a hot field)   + len ×2, cap ×1, `i < n` ×2 after
//                          `find_id` had already proved the index in range
//   put_field_hint  HIT  (interp + compiled putfield)   len ×4, cap ×2, `hi < n` tested ×4
//   get_field_by_id      (by-id + every `Syn` read,     len ×4, cap ×3, `i < n` ×2 post-proof,
//                         `elems_len` asks twice)         PLUS an `ids()` slice built
//                          unconditionally whose only reader is the DISARMED `RAVA_DIAG` branch
//                          — a fact derived before the test that decides whether it is needed
//   get_field / put_field_by_id (by-name + `Syn` writes) len ×3, cap ×2, `i < n` ×2 post-proof
//
// AFTER: `BlobView::of` reads the header ONCE — one `movzx` for `n`, one for `cap` — computes
// the three section pointers once, and the access threads that view to both arms. The index a
// lookup returns is proved `< n` exactly once, by the lookup, and the cell/kind reads that
// follow are two raw loads with no second bound and no landing pad.
//
// ⚠**THE VIEW ONLY REMOVES DERIVATIONS; IT NEVER DECIDES ANYTHING.** `n`, the pointers and the
// values read through them are bit-identical to what the slices produced — the offsets come from
// `FieldBlob`'s own `cells_off`/`ids_off`/`kinds_off`, so a layout change moves both halves
// together. `BlobView::of` re-derives the same four facts through the ORIGINAL accessors and
// `debug_assert`s equality on EVERY call in a debug build, which is the discipline that made the
// previous round safe: the cheap form may differ from the original in no observable way.
//
// ⚠**AND IT IS NEVER HELD ACROSS A MUTATION THAT CAN MOVE THE BLOCK.** `push` reallocs; every
// site below builds its view, uses it only on the arms that do NOT push, and reads nothing
// through it after a push. The view is a plain `Copy` bag of raw pointers — it borrows nothing,
// so it cannot silently keep a stale block alive either.
//
// Kill switch: `RAVA_NO_FAST_FIELDDERIVE=1` (or the blanket `RAVA_NO_EMIT_MORE`) sets
// `HG_NODERIVE` and every site takes its original slice-per-section arm verbatim. ⚠**With the
// switch on, `BlobView::of`'s two header loads are still paid and then not read** — the same
// deliberate residue `HG_LEGACY` leaves for the gate word (it too still builds the word). The
// switch restores the SHAPE being measured (slice per section, bounds test per index), not the
// last two loads, and the ANSWERS never differed either way.
#[derive(Clone, Copy)]
pub(crate) struct BlobView {
    /// Live slot count — the header word at +0. The bound of ids, cells AND kinds alike.
    n: usize,
    ids: *mut u32,
    cells: *mut i64,
    kinds: *mut u8,
}

impl BlobView {
    /// The four facts, derived once. Two header loads and two multiplies; no branch.
    #[inline(always)]
    fn of(fb: &FieldBlob) -> BlobView {
        // SAFETY: identical loads to `FieldBlob::len`/`cap`, off the same `NonNull` base. For
        // the shared read-only sentinel `cap == 0` and every section offset collapses to +8,
        // i.e. one-past-the-end of the 8-byte `EMPTY_HDR` — a valid address to FORM (this is
        // exactly what `FieldBlob::sentinel_ptrs` already does) and never dereferenced, because
        // `n == 0` there and every accessor below requires `i < n`.
        let base = fb.0.as_ptr();
        let n = unsafe { *(base as *const u32) as usize };
        let cap = unsafe { *(base.add(4) as *const u32) as usize };
        let v = unsafe {
            BlobView {
                n,
                ids: base.add(FieldBlob::ids_off(cap)) as *mut u32,
                cells: base.add(FieldBlob::cells_off()) as *mut i64,
                kinds: base.add(FieldBlob::kinds_off(cap)),
            }
        };
        // The prefilter discipline of the previous round, applied to a derivation: prove on
        // every call in a debug build that the cheap form names the same four facts as the
        // accessors it replaces. Compiled out entirely in release.
        debug_assert_eq!(v.n, fb.len(), "BlobView count disagrees with FieldBlob::len");
        debug_assert_eq!(v.ids as *const u32, fb.ids_ptr(), "BlobView ids base drifted");
        debug_assert_eq!(v.cells as *const i64, fb.cells_ptr(), "BlobView cells base drifted");
        debug_assert_eq!(v.kinds as *const u8, fb.kinds_ptr(), "BlobView kinds base drifted");
        v
    }

    /// Field id in slot `i`.
    ///
    /// SAFETY: `i < self.n`, which is `FieldBlob::len()` — the exact bound `ids()` gives its
    /// slice. Every caller has just tested it or has it from a lookup that proved it.
    #[inline(always)]
    unsafe fn id_at(&self, i: usize) -> u32 {
        // Edition-2021 `unsafe fn`: the body is already an unsafe context (matching this
        // crate's existing `eq_sse2`/`alloc_in_arena` style), so no nested block.
        debug_assert!(i < self.n);
        *self.ids.add(i)
    }

    /// Cell + kind of slot `i` — the pair every read decodes and every write stores.
    ///
    /// SAFETY: as `id_at`.
    #[inline(always)]
    unsafe fn cell_kind_at(&self, i: usize) -> (i64, u8) {
        debug_assert!(i < self.n);
        (*self.cells.add(i), *self.kinds.add(i))
    }

    /// Overwrite slot `i` in place.
    ///
    /// SAFETY: as `id_at`, plus the `&mut FieldBlob` the caller passes: it proves unique access
    /// to the block for the duration, and it proves the block is the one this view was derived
    /// from (no `push`/`realloc_to` can have intervened — those need the same `&mut`). The
    /// pointers carry the provenance of `FieldBlob`'s own `NonNull<u8>`, which is `*mut`, so
    /// writing through them is the identical operation `cells_mut`/`kinds_mut` perform.
    #[inline(always)]
    unsafe fn store_at(&self, _fb: &mut FieldBlob, i: usize, cell: i64, kind: u8) {
        debug_assert!(i < self.n);
        *self.cells.add(i) = cell;
        *self.kinds.add(i) = kind;
    }
}

impl Heap {
    /// Allocate the global field-hint table once (before any compiled code runs)
    /// and publish its base in `jit_ctx`. Fixed size → the base never moves, so a
    /// single call suffices; idempotent. `u32::MAX` is the empty sentinel (always
    /// `>= count` → forces a scan on first access to each bucket).
    pub fn ensure_field_hints(&mut self) {
        if self.field_hints.is_empty() {
            self.field_hints = vec![u32::MAX; JIT_FIELD_HINT_N];
            self.jit_ctx.field_hint_base = self.field_hints.as_ptr() as usize;
        }
    }

    /// Encode a `Value` into its flat `(cell, kind)` field representation. Str /
    /// StdOut / StdErr go through the append-only `field_box` (hence `&mut self`).
    pub(crate) fn encode_field(&self, v: &Value) -> (i64, u8) {
        match v {
            Value::Int(i) => (*i as i64, FK_INT),
            Value::Long(l) => (*l, FK_LONG),
            Value::Float(f) => (f.to_bits() as u32 as i64, FK_FLOAT),
            Value::Double(d) => (d.to_bits() as i64, FK_DOUBLE),
            // Reference cells are stored TAGGED (same encoding as the JIT operand
            // cell) so native getfield can push them directly without reading the
            // kind. The kind is still recorded for decode_field / putfield.
            Value::Ref(h) => (*h as i64 | REF_TAG, FK_REF),
            Value::Array(h) => (*h as i64 | ARRAY_TAG, FK_ARRAY),
            Value::Null => (0, FK_NULL),
            // Arena object: store the raw 8-aligned pointer; the moving GC forwards
            // the cell in place. Bypasses the append-only field_box (leak fix).
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => (p.addr() as i64, FK_OBJ),
            _ => {
                // GILFREE-RINGS: a `field_box` PUSH from a gil-free worker is the
                // same defect class `put_object` was hardened against — a shared Vec
                // grow (realloc + torn index) under concurrent readers. Route the
                // rings lane into its own pre-reserved, disjoint slice
                // (`reserve_field_box_range` at plan time); exhaustion aborts the
                // task rather than falling back to the shared push. Keyed on
                // `in_lane` so the pre-existing COW forks keep their exact old
                // behaviour (byte-identical with the flag on or off).
                #[cfg(feature = "next-gen-gc")]
                if crate::gilfree_cf::in_lane() {
                    return match crate::parallel::field_box_take() {
                        Some(idx) => {
                            self.field_box.write().unwrap_or_else(|e| e.into_inner())[idx as usize] = v.clone();
                            (i64::from(idx) | ABOX_TAG, FK_BOX)
                        }
                        None => {
                            crate::gilfree_cf::note_stub_abort("field-box slice exhausted");
                            crate::parallel::request_abort();
                            (0, FK_NULL)
                        }
                    };
                }
                let mut fb = self.field_box.write().unwrap_or_else(|e| e.into_inner());
                let idx = fb.len();
                fb.push(v.clone());
                drop(fb);
                self.bytes.fetch_add(SLOT_BYTES, std::sync::atomic::Ordering::Relaxed);
                (idx as i64 | ABOX_TAG, FK_BOX)
            }
        }
    }

    /// Decode a flat `(cell, kind)` field back to a `Value`.
    pub(crate) fn decode_field(&self, cell: i64, kind: u8) -> Value {
        match kind {
            FK_INT => Value::Int(cell as i32),
            FK_LONG => Value::Long(cell),
            FK_FLOAT => Value::Float(f32::from_bits(cell as u32)),
            FK_DOUBLE => Value::Double(f64::from_bits(cell as u64)),
            FK_REF => Value::Ref(cell as u32), // low 32 bits = handle (REF_TAG masked)
            FK_ARRAY => Value::Array(cell as u32),
            FK_NULL => Value::Null,
            #[cfg(feature = "next-gen-gc")]
            FK_OBJ => Value::Obj(unsafe { crate::arena::ArenaPtr::from_addr(cell as u64) }),
            _ => self
                .field_box
                .read().unwrap_or_else(|e| e.into_inner())
                .get((cell & 0xFFFF_FFFF) as usize)
                .cloned()
                .unwrap_or(Value::Null),
        }
    }

    /// Decode slot `i` of `o`'s field block through the view derived once at the top of the
    /// access. The bound `i < len` is the caller's — either a lookup returned `i` (and every
    /// lookup returns only indices inside `ids()`), or the caller tested `i < bv.n` itself.
    /// Was: two slice constructions (3 header loads) and two more bounds tests of that same
    /// already-proved fact, each with its own panic landing pad.
    ///
    /// `fast == false` (kill switch) takes the original expression verbatim.
    #[inline(always)]
    fn decode_slot(&self, o: &Object, bv: BlobView, fast: bool, i: usize) -> Value {
        if fast {
            // SAFETY: `i < bv.n == o.fields.len()`, established by the caller as above; `bv` was
            // derived from THIS blob and `&self` forbids any mutation since.
            let (cell, kind) = unsafe { bv.cell_kind_at(i) };
            self.decode_field(cell, kind)
        } else {
            self.decode_field(o.fields.cells()[i], o.fields.kinds()[i])
        }
    }

    /// Store into slot `i` of `o`'s field block through the once-derived view. Same bound and
    /// same provenance argument as `decode_slot`; the `&mut Object` proves unique access and
    /// proves no `push` (the only reallocating mutator) has run since the view was taken.
    #[inline(always)]
    fn store_slot(o: &mut Object, bv: BlobView, fast: bool, i: usize, cell: i64, kind: u8) {
        if fast {
            // SAFETY: see `BlobView::store_at`.
            unsafe { bv.store_at(&mut o.fields, i, cell, kind) }
        } else {
            o.fields.cells_mut()[i] = cell;
            o.fields.kinds_mut()[i] = kind;
        }
    }

    /// `get_field` for one of the constant synthetic names (`heap::Syn`). Identical in every
    /// respect except how the field id is obtained: by discriminant instead of by string. See
    /// `syn_field_id` for why that is worth a separate entry point.
    pub fn get_field_syn(&self, handle: u32, s: crate::heap::Syn) -> Value {
        let Some(id) = self.syn_field_id(s) else { return Value::Null };
        self.get_field_by_id(handle, id)
    }

    /// The half of `get_field` that follows the id — the object slot and the field scan.
    pub fn get_field_by_id(&self, handle: u32, id: u32) -> Value {
        // ⭐**A LINEAR SCAN PER FIELD READ.** A JVM reads a field at a constant offset computed
        // once at class-load time; this walks the object's id array until it matches. `heap` is
        // 12.7% of host samples — the largest unnamed line after the natives — and field access
        // is the most frequent operation an object program performs, so both the price and the
        // scan LENGTH are worth knowing before anything is redesigned.
        //
        // ⭐**AND THE INSTRUMENT WAS COSTING THIS PATH TWO ACQUIRE PAIRS AND LIVE DROP GLUE.**
        // `field_read()` is a `OnceLock<Meter>` and `field_scan_len()` a `OnceLock<Histogram>`:
        // disarmed — which is every product boot — each was still an acquire load of the once
        // state, a branch, a second load to deref, and a 24-byte handle copy; and the
        // `MeterGuard` bound to `_t` was constructed on the stack and its `Drop` still ran at
        // every return. That is instrument plumbing on the by-id read funnel (`get_field_syn`
        // routes here, and `elems_len` asks twice per logical-length query), paid to record
        // nothing. This is the identical fix `put_array` already carries: one bit of the heap
        // gate word, and the block below is EXACTLY what it was when armed. Observationally
        // identical because `dump` returns early when nothing is armed and no counter can be
        // armed while the selector is empty. 2 acquire pairs + a guard construction → 1 relaxed
        // word load shared with the rest of this file's gates.
        //
        // ⚠`_t` stays a NAMED binding, never a bare `_`: `let _ = m.start()` drops the guard
        // immediately and times nothing, which is the one way to use `Meter` wrong.
        let gw = heap_gates();
        let armed = hg_diag(gw);
        let _t = if armed { Some(crate::diag::field_read().start()) } else { None };
        let Some(o) = self.get(handle) else {
            return Value::Null;
        };
        // ⭐**THE BLOCK IS DERIVED ONCE HERE AND THREADED TO THE TAIL.** This line used to be
        // `let ids = o.fields.ids()` — a slice construction (a `len` load, a `cap` load and the
        // `8 + 8·cap` offset) whose ONLY reader is the `if armed` branch below, i.e. a fact
        // derived before the test that decides whether it is needed, on a path where the test
        // is false on every product boot. `bv.n` IS `ids.len()`, so the armed record is
        // unchanged, and the same `bv` now serves the decode at the bottom, which was
        // re-deriving `len` twice more and `cap` once more of its own.
        let bv = BlobView::of(&o.fields);
        let fast = hg_derive(gw);
        if armed {
            crate::diag::field_scan_len().record(bv.n as u64);
        }
        // ⛔**A SLOT CACHE WAS BUILT HERE AND MEASURED WORSE, TWICE.** `(class word, field id) ->
        // slot`, verified on use exactly like the machine-code PIC's class compare, hit **62%**
        // of 26 M reads a boot — and made the read SLOWER both times:
        //
        // ```text
        // plain scan            30.7 ns   2.41% of boot
        // cache, 16384 slots    34.1 ns   2.69%     (196 KB, far past L1)
        // cache, 512 slots      35.9 ns   2.78%     (6 KB, resident)
        // ```
        //
        // Shrinking it into L1 did not help, so the size was not the fault: **a two-to-four entry
        // scan over an array the caller has already loaded beats a hash plus a table load plus a
        // verify.** The cost of `get_field_by_id` is not the walk — it is the object-slot lookup
        // and the decode around it, which no cache on the id array can touch.
        //
        // A JVM wins here by never touching an id array at all: the offset is baked into the
        // instruction at class-load. That is a layout change, not a cache.
        // AND THE ENGINE ALREADY OWNED THE ANSWER -- THE COMPILED HALF HAS HAD IT ALL ALONG.
        // `Heap::field_hints` is a global `fid & 8191` bucketed slot table that every emitted
        // getfield reads at `ctx+24` and `publish_field_hint` refills. The HOST half -- ~92% of
        // a product boot's samples -- sat next to it re-walking the id array. `find_id` reads
        // that same table, verifies `ids[h] == id` exactly as the emitted guard does, and
        // publishes on a miss, so both halves now warm ONE memo instead of one of them warming
        // none. WAS O(fields-per-object) per read: 32+ on the dominant key class, whose
        // `$key`/`$val`/`$keystr` are APPENDED PAST the declared layout and so were always at
        // the far end of the walk. IS O(1) load + verify, scanning only on a stale/colliding
        // entry. No new table (`FieldBlob::find_id`); kill switch RAVA_NO_FAST_FIELDID.
        match o.fields.find_id(id, self.jit_ctx.field_hint_base) {
            // `find_id` has PROVED `i < ids.len()`; the cell and kind reads no longer re-prove
            // it, and no longer rebuild the two slices to do so.
            Some(i) => self.decode_slot(o, bv, fast, i as usize),
            None => Value::Null,
        }
    }

    /// The LOGICAL length of an `$elems`-backed collection.
    ///
    /// ⭐**The shared convention gained an optional size, and this is where readers honour it.**
    /// Natives take an `$elems` array's LENGTH as the size and get amortised growth for free
    /// because that array is a Rust `Vec`. Guest bytecode cannot grow a Java array, so a
    /// `java.util` class written as bytecode either reallocates on every `add` (O(n) — a boot
    /// went 40 s -> 166 s) or pays a native call per `add` (measured 593 ns/add, i.e. the very
    /// seam the migration exists to remove).
    ///
    /// So an overriding class may keep spare capacity in `$elems` and record the real length in
    /// `$size`. Absent `$size` — every native-fabricated collection, which is all of them today
    /// — this answers the array length exactly as before, so the default path is unchanged.
    /// The logical CONTENTS of an `$elems`-backed collection — `array_values` truncated to
    /// `elems_len`. The one call every raw `$elems` reader should use, because an overriding
    /// class keeps spare capacity and the trailing slots are not elements.
    pub(crate) fn elems_values(&self, handle: u32, arr: u32) -> Vec<Value> {
        let mut v = self.array_values(arr).unwrap_or_default();
        v.truncate(self.elems_len(handle, arr));
        v
    }

    pub(crate) fn elems_len(&self, handle: u32, arr: u32) -> usize {
        let cap = self.array_len(arr).unwrap_or(0);
        // ⛔**THE DEFAULT PATH MUST NOT PAY FOR THIS, AND THE FIRST VERSION DID.** `iterate_object`
        // is documented as the one place every drain of an `$elems`-backed collection passes
        // through, and two extra field lookups there — for fields no ordinary class declares, so
        // both MISS — took the product boot from 40 s to a 240 s timeout with the CPU pegged.
        // ⚠4 gates and 20 probes stayed green through all of it: only the boot catches this.
        // With no class overridden there is nothing to ask, so ask nothing.
        // ⛔**AND IT MUST BE SCOPED TO THE OVERRIDDEN CLASS, NOT TO "OVERRIDING IS ON".** Every
        // `$elems` collection shares this field — sets, iterators, stream results — and the
        // natives grow several of them IN PLACE (`array_push`, `coll_op`'s O(1) arms) without
        // touching `$size`. Maintaining `$size` for those would leave it stale below the array's
        // real length and they would silently LOSE elements. Only a class that actually has
        // bytecode of its own plays by this convention.
        if !crate::class_loader::any_override()
            || !self
                .class_of(handle)
                .is_some_and(crate::class_loader::has_bytecode)
        {
            return cap;
        }
        // ⛔**`$size` ALONE CANNOT BE THE TEST, AND ZERO IS WHY.** Once an overriding class
        // DECLARES `$size` the field exists in that class's layout for every instance of it,
        // including the ones natives fabricate and never write — where it reads a default 0.
        // Trusting it there reported every native-built list as EMPTY, and MC died in 0.6 s with
        // a `ConcurrentModificationException` from the fail-fast witness. `$init` is written by
        // the guest constructors and by nothing else, so it is the only sound discriminator
        // (third time this session that a legitimate 0 has been mistaken for "absent").
        // BY CONSTANT, NOT BY STRING. `$init`/`$size` are two of the eighteen names `Syn`
        // exists to resolve once per process; reaching them through the by-name entry hashed
        // 5-6 characters and probed the interner on EVERY logical-length query -- and this is
        // the funnel every drain of an `$elems`-backed collection passes through. Per-call work
        // that is a constant of the PROCESS. Identical resolution (`syn_field_id` reads the same
        // `field_ids` map `field_id_existing` does), so a missing name still answers Null.
        match self.get_field_syn(handle, crate::heap::Syn::Init) {
            Value::Int(1) => match self.get_field_syn(handle, crate::heap::Syn::Size) {
                Value::Int(n) if n >= 0 && (n as usize) <= cap => n as usize,
                _ => cap,
            },
            _ => cap,
        }
    }

    pub fn get_field(&self, handle: u32, name: &str) -> Value {
        let Some(id) = self.field_id_existing(name) else { return Value::Null };
        let Some(o) = self.get(handle) else {
            return Value::Null;
        };
        // GATE FOLD: the verify block and the read-flip block below each used to open
        // with their own `OnceLock` acquire pair — 2 per by-name field READ, both to
        // learn that a default-OFF migration flag is off. One relaxed byte now answers
        // both, and the two tests below are register tests on `gw`.
        // ⭐**AND IT IS READ AT THE TOP NOW, NOT INSIDE THE HIT ARM**, because the same word
        // also carries `hg_derive` — the one-derivation block view, which has to exist BEFORE
        // the lookup that uses it. One relaxed load either way; it just moved up.
        let gw = heap_gates();
        // The block's count, capacity and three section bases: derived ONCE. The decode below
        // was rebuilding `cells()` (a `len` load) and `kinds()` (a `len` and a `cap` load) and
        // bounds-testing an index `find_id` had already proved in range.
        let bv = BlobView::of(&o.fields);
        let fast = hg_derive(gw);
        // Same memo as `get_field_by_id` (see there for why it is the compiled path's own
        // table and costs no new bytes): O(fields-per-object) -> O(1) load + verify.
        if let Some(i) = o.fields.find_id(id, self.jit_ctx.field_hint_base) {
            let i = i as usize;
            {
                let v = self.decode_slot(o, bv, fast, i);
                // B4-validation (RAVA_PINNED_VERIFY, default OFF): assert-only —
                // the returned value is the authoritative FieldBlob decode; this
                // only cross-checks that the pinned mirror cell at the class-layout
                // slot decodes (via the arena path) to the SAME value, proving the
                // dual-store consistent by live execution without changing behavior.
                #[cfg(feature = "next-gen-gc")]
                if hg_verify(gw) {
                    let mirror = crate::nextgen::pinned_mirror_get(handle);
                    if mirror != 0 {
                        if let Some(layout) = self.layouts.read().unwrap_or_else(|e| e.into_inner()).get(o.class.as_ref()).cloned() {
                            if let Some(slot) = self.layout_slot(o.class.as_ref(), id) {
                                let cell = unsafe {
                                    *((mirror as *const u8)
                                        .add(crate::nextgen::HDR + slot * 8)
                                        as *const i64)
                                };
                                let mv = crate::opcodes::decode_arena_cell(cell, layout[slot].1);
                                assert!(
                                    crate::opcodes::ref_equal(&mv, &v),
                                    "pinned mirror mismatch h={handle} field={name}: \
                                     mirror={mv:?} vs blob={v:?}"
                                );
                            }
                        }
                    }
                }
                // B4 read-flip (RAVA_PINNED_READ, default OFF): SERVE the read from
                // the pinned mirror — the first execution use of the unified object
                // as the source of truth. A clean canonical run proves it serves
                // reads correctly (incl. no ref-cell staleness between writes).
                #[cfg(feature = "next-gen-gc")]
                if hg_pread(gw) {
                    let mirror = crate::nextgen::pinned_mirror_get(handle);
                    if mirror != 0 {
                        if let Some(layout) = self.layouts.read().unwrap_or_else(|e| e.into_inner()).get(o.class.as_ref()).cloned() {
                            if let Some(slot) = self.layout_slot(o.class.as_ref(), id) {
                                let cell = unsafe {
                                    *((mirror as *const u8)
                                        .add(crate::nextgen::HDR + slot * 8)
                                        as *const i64)
                                };
                                return crate::opcodes::decode_arena_cell(cell, layout[slot].1);
                            }
                        }
                    }
                }
                return v;
            }
        }
        Value::Null
    }

    /// B3 dual-store, single-slot: mirror one field WRITE into the pinned object
    /// (the in-place overwrite arms of put_field/put_field_hint never reach
    /// sync_obj_desc — only a push does — so without this the mirror goes stale on
    /// every overwritten field). encode_arena_cell(value) ≡ the sync bridge's
    /// decode_field→encode_arena_cell for the same value. No-op without the flag /
    /// a mirror / a layout slot.
    /// Encode `v` for a pinned-mirror cell of LAYOUT kind `layout_kind` — the
    /// arena's own convention. A primitive slot (kinds 0..=3) stores raw bits. A
    /// REF slot must hold only null / raw ptr / (h<<2)|1 / |2 / box|3 — an
    /// identity-boxed primitive (a `Value::Int` in a ref-typed field, rava's
    /// boxing model) or Str/sentinel is BOXED like `jit_abi::unify_pack`;
    /// flattening it raw (encode_arena_cell's Int arm) would decode as a garbage
    /// tagged handle.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn encode_mirror_cell(v: &Value, layout_kind: u8) -> i64 {
        if layout_kind <= 3 {
            return crate::opcodes::encode_arena_cell(v);
        }
        match v {
            Value::Null => 0,
            Value::Obj(p) => p.addr() as i64,
            Value::Ref(h) => ((*h as i64) << 2) | 1,
            Value::Array(a) => ((*a as i64) << 2) | 2,
            other => ((crate::nextgen::arena_box_push(other.clone()) as i64) << 2) | 3,
        }
    }

    /// ⚠**THE GATE IS THE CALLER'S NOW.** This used to open with its own
    /// `pinned_mirror_enabled()` acquire pair, which meant every field WRITE paid it — and, worse,
    /// paid it AFTER the caller had already cloned the object's class `Arc<str>` purely to have
    /// something to hand this function. Both write funnels (`put_field_by_id`, `put_field_hint`)
    /// now test `hg_mirror` on the word they already hold and skip the clone and the call
    /// together. Same predicate, same order, one fewer acquire and two fewer atomic RMWs per
    /// write. Only those two funnels call this.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn pinned_mirror_store(&self, handle: u32, class: &str, id: u32, value: &Value) {
        let mirror = crate::nextgen::pinned_mirror_get(handle);
        if mirror == 0 {
            return;
        }
        let Some(layout) = self.layouts.read().unwrap_or_else(|e| e.into_inner()).get(class).cloned() else { return };
        let Some(slot) = self.layout_slot(class, id) else { return };
        let cell = Self::encode_mirror_cell(value, layout[slot].1);
        unsafe {
            *((mirror as *mut u8).add(crate::nextgen::HDR + slot * 8) as *mut i64) = cell;
        }
    }

    pub fn put_field(&self, handle: u32, name: &str, value: Value) {
        // Census only (`--features contract-stat`); folds to nothing otherwise.
        crate::contract_stat::synth_stamp(name);
        // ⭐**A NATIVE THAT REPLACES `$elems` INVALIDATES THE GUEST'S CACHED LENGTH.**
        //
        // The `$size`/`$init` convention is symmetric or it is wrong. Reads honour `$size`
        // (`elems_len`), but natives keep mutating collections by REPLACING the backing array —
        // `store_elems`, the collectors, the map views, `Files.list` — and they do it through
        // this name-keyed setter while never touching `$size`. A guest class that had already
        // adopted the old array then reported the OLD length: MC's datapack load died with
        // `IndexOutOfBoundsException: Index 0 out of bounds for length 0` on a list a native had
        // just filled.
        //
        // Clearing `$init` makes the guest re-adopt the array's length on its next touch, which
        // is exactly the natives' own convention. Guest bytecode writes its fields by ID through
        // the arena cells, not through this name-keyed path, so its own growth is unaffected.
        if crate::class_loader::any_override()
            && name == "$elems"
            && self
                .class_of(handle)
                .is_some_and(crate::class_loader::has_bytecode)
        {
            // Maintain the length rather than invalidate it: the natives' convention is that the
            // array they just stored IS the collection, so its length IS the size. Recurses
            // exactly once each — neither inner name is `$elems`.
            let n = match &value {
                Value::Array(a) => self.array_len(*a).unwrap_or(0),
                _ => 0,
            };
            // By constant, not by string: `put_field_syn` resolves `$size`/`$init` through the
            // `Syn` cache instead of hashing the name on every native array replacement.
            // Neither is `$elems`, so each recurses exactly once, as before.
            self.put_field_syn(handle, crate::heap::Syn::Size, Value::Int(n as i32));
            self.put_field_syn(handle, crate::heap::Syn::Init, Value::Int(1));
        }
        let id = self.field_id(name);
        self.put_field_by_id(handle, id, value);
    }

    /// `put_field` for a SYNTHETIC field, by its cached id instead of by hashing its name.
    ///
    /// ⭐**98 reads and 47 writes of `$elems`/`$map`/`$keystr`/… named their field with a STRING,
    /// so every one hashed 6-8 characters to reach a constant.** `Syn` already carried the
    /// resolved ids for the read side (`get_field_syn`); this is the missing write half.
    ///
    /// ⚠**The `$elems` invariant below is NOT optional and NOT a fast-path detail.** `put_field`
    /// maintains `$size`/`$init` when a native replaces the backing array of a class whose real
    /// bytecode is loaded, and its own comment records what losing it costs: MC's datapack load
    /// died with `IndexOutOfBoundsException: Index 0 out of bounds for length 0`. It is
    /// replicated here against the enum rather than against the name — same condition, same two
    /// writes, no string compare.
    ///
    /// ⚠And the id may not EXIST yet: a write can be a field's first appearance, and
    /// `syn_field_id` only reads the intern table while `field_id` creates. An unresolved id
    /// falls back to the by-name path, which interns it, so the next call takes the fast one.
    pub fn put_field_syn(&self, handle: u32, s: crate::heap::Syn, value: Value) {
        let name = crate::heap::intern::SYN_NAMES[s as usize];
        crate::contract_stat::synth_stamp(name);
        if matches!(s, crate::heap::Syn::Elems)
            && crate::class_loader::any_override()
            && self
                .class_of(handle)
                .is_some_and(crate::class_loader::has_bytecode)
        {
            let n = match &value {
                Value::Array(a) => self.array_len(*a).unwrap_or(0),
                _ => 0,
            };
            self.put_field_syn(handle, crate::heap::Syn::Size, Value::Int(n as i32));
            self.put_field_syn(handle, crate::heap::Syn::Init, Value::Int(1));
        }
        match self.syn_field_id(s) {
            Some(id) => self.put_field_by_id(handle, id, value),
            None => {
                let id = self.field_id(name);
                self.put_field_by_id(handle, id, value)
            }
        }
    }

    /// The half of `put_field` that follows the id — the object slot, the blob write and the
    /// mirror publish. Shared by the by-name and by-`Syn` entries so they cannot drift.
    fn put_field_by_id(&self, handle: u32, id: u32, value: Value) {
        let (cell, kind) = self.encode_field(&value);
        // Read the hint-table base BEFORE the exclusive borrow of the object slot below.
        let hb = self.jit_ctx.field_hint_base;
        // ⭐**ONE WORD FOR THE WHOLE WRITE.** The mirror publish and the shadow card barrier used
        // to open with an acquire-ordered `OnceLock` pair each — 2 per by-name / by-`Syn` field
        // write, both answering a constant of the process. One relaxed byte, read once here, at
        // the natural entry, and bit-tested below. It also carries `hg_derive`, so it is read
        // unconditionally now rather than only under `next-gen-gc` — still one relaxed load.
        let gw = heap_gates();
        let fast = hg_derive(gw);
        let mut pushed = false;
        // ⭐**AND THE `Arc` CLONE IS NOW THE MIRROR'S, NOT EVERY WRITE'S.** `pinned_mirror_store`
        // needs `&str`, but it is called after the exclusive borrow of the object slot has ended,
        // so the class name had to be cloned OUT of the slot — two atomic read-modify-writes (the
        // increment here, the decrement at the end of the function) on EVERY field write, to feed
        // a function that returns immediately unless `RAVA_PINNED_MIRROR` is set. It is not set on
        // any product boot. The borrow genuinely cannot be proven past `objects.get_mut` — the
        // `&mut self` reborrow for `sync_obj_desc_incremental` sits between — so the clone stays,
        // but only on the armed path.
        #[cfg(feature = "next-gen-gc")]
        let mirror_on = hg_mirror(gw);
        #[cfg(feature = "next-gen-gc")]
        let mut cls: Option<Arc<str>> = None;
        if let Some(o) = self.objects.write().unwrap_or_else(|e| e.into_inner()).get_mut(handle as usize).and_then(ObjectSlot::get_mut) {
            #[cfg(feature = "next-gen-gc")]
            if mirror_on {
                cls = Some(o.class.clone());
            }
            // The block's count/cap/section bases, derived ONCE — the store below was rebuilding
            // `cells_mut()` (a `len` load) and `kinds_mut()` (a `len` and a `cap` load) and
            // bounds-testing an index `find_id` had already proved in range. Taken BEFORE
            // `find_id` (which only reads) and read only on the arm that does NOT `push` — a
            // push can realloc the block and would invalidate it.
            let bv = BlobView::of(&o.fields);
            // O(fields-per-object) -> O(1) hint + verify, same table and same guard the
            // emitted putfield uses. A miss publishes, so the next write (here or in compiled
            // code) lands direct.
            if let Some(i) = o.fields.find_id(id, hb) {
                Self::store_slot(o, bv, fast, i as usize, cell, kind);
            } else {
                o.fields.push(id, cell, kind);
                pushed = true;
            }
        }
        if pushed {
            self.bytes.fetch_add(13, std::sync::atomic::Ordering::Relaxed);
            // Incremental: mirror the single pushed field via pinned_mirror_store
            // below, not a whole-object re-encode here (O(1) instead of O(n²)).
            self.sync_obj_desc_incremental(handle);
        }
        #[cfg(feature = "next-gen-gc")]
        if let Some(c) = cls {
            self.pinned_mirror_store(handle, &c, id, &value);
        }
        // SHADOW write barrier (dry-run, RAVA_SHADOW_GC): a reference stored into an object is
        // the old→young pointer the copying GC must track. Mark the covering card.
        // Second consumer of the one word read at the top: this was the write path's other
        // acquire pair.
        #[cfg(feature = "next-gen-gc")]
        if hg_shadow(gw) && matches!(value, Value::Ref(_) | Value::Array(_) | Value::Obj(_)) {
            // A `Value::Obj` (raw arena pointer) stored into a handle object is the
            // SAME old→young edge as a Ref/Array handle — the card barrier must
            // record it or the moving collector can miss the arena object as a root.
            crate::nextgen::shadow().card_mark(handle);
        }
    }

    /// SHADOW root validation (dry-run): is `v` a valid, aligned, LIVE heap object
    /// — i.e. would the copying GC's root-trace accept it? Mirrors what the real
    /// mark phase relies on. Read-only; never mutates.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn shadow_validate(&self, v: &Value) -> bool {
        match v {
            Value::Ref(h) => {
                let i = *h as usize;
                i < self.objects.read().unwrap_or_else(|e| e.into_inner()).len()
                    && self.obj_alive.lock().unwrap_or_else(|e| e.into_inner()).get(i).copied().unwrap_or(false)
                    && self.get(i as u32).is_some_and(|o| !o.class.is_empty())
            }
            Value::Array(h) => {
                let i = *h as usize;
                let arrays = self.arrays.read().unwrap_or_else(|e| e.into_inner());
                i < arrays.len() && self.arr_alive.read().unwrap_or_else(|e| e.into_inner()).get(i).copied().unwrap_or(false)
            }
            // Primitives / null are trivially valid non-roots.
            _ => true,
        }
    }

    /// Read field `id`, trying `hint` slot first (a JVM-style cached field
    /// offset). Returns the value and the actual slot. The `fields[hint].0 == id`
    /// check makes a stale hint self-correcting — a polymorphic call site whose
    /// receiver lays the field out elsewhere just falls back to the scan.
    /// B4 read-flip helper: when RAVA_PINNED_READ is on and this handle has a
    /// pinned mirror with a layout slot for `id`, decode the value from the MIRROR
    /// cell (arena path) instead of the FieldBlob. Returns None when the mirror
    /// cannot serve (no flag / no mirror / no layout slot) → caller uses FieldBlob.
    ///
    /// ⚠**THE READ GATE IS THE CALLER'S NOW, AND THAT IS THE POINT.** This used to open with its
    /// own `pinned_read_enabled()` acquire pair, so the interpreter's getfield funnel made a
    /// NON-INLINED call, on every single field read, whose entire job on the default path was to
    /// load a `OnceLock` and return `None`. `get_field_hint` now bit-tests `hg_pread` on the word
    /// it already holds and does not make the call at all; the `gw` it passes in serves the
    /// verify arm below, which was this function's second acquire pair. Only fields.rs calls this.
    #[cfg(feature = "next-gen-gc")]
    #[cold]
    #[inline(never)]
    pub(crate) fn pinned_read_field(
        &self,
        gw: u16,
        handle: u32,
        class: &str,
        id: u32,
    ) -> Option<Value> {
        let mirror = crate::nextgen::pinned_mirror_get(handle);
        if mirror == 0 {
            return None;
        }
        let layout = self.layouts.read().unwrap_or_else(|e| e.into_inner()).get(class).cloned()?;
        let slot = self.layout_slot(class, id)?;
        let cell = unsafe {
            *((mirror as *const u8).add(crate::nextgen::HDR + slot * 8) as *const i64)
        };
        let mv = crate::opcodes::decode_arena_cell(cell, layout[slot].1);
        // DIVERGE-HUNT (READ+VERIFY together): compare the mirror value against the
        // authoritative FieldBlob; on mismatch LOG the exact class+field and serve
        // the blob — the run completes and the log enumerates every remaining
        // unmirrored write path in one pass (instead of crashing at the first).
        if hg_verify(gw) {
            let bv = self.get(handle)
                .and_then(|o| {
                    // `scan_id`, not `find_id`: this is the divergence ARBITER, so it must read
                    // the blob with no memo in the path at all.
                    o.fields.scan_id(id).map(|i| {
                        let i = i as usize;
                        self.decode_field(o.fields.cells()[i], o.fields.kinds()[i])
                    })
                })
                .unwrap_or(Value::Null);
            if !crate::opcodes::ref_equal(&mv, &bv) {
                let f = self.field_name(id).unwrap_or_else(|| "?".to_string());
                eprintln!("[MIRROR-DIVERGE] {class}.{f} (id={id}) mirror={mv:?} blob={bv:?}");
                return Some(bv);
            }
        }
        Some(mv)
    }

    /// Slot carrying field `id` on `handle`, or None if the object does not have that field.
    ///
    /// `get_field`/`get_field_hint` return `Value::Null` for BOTH "no such field" and "the field
    /// holds null", so neither can be used to test SHAPE. That distinction is what lets a caller
    /// key on our own synthetic namespace (`$key`, `$val`, `$elems`, ...) instead of on a class
    /// NAME - which matters because naming a third-party class would be adapting the engine to a
    /// particular jar. Same hint contract as `get_field_hint`: try the hint slot, else scan.
    pub(crate) fn field_slot(&self, handle: u32, id: u32, hint: u32) -> Option<u32> {
        let o = self.get(handle)?;
        let ids = o.fields.ids();
        let hi = hint as usize;
        if hi < ids.len() && ids[hi] == id {
            return Some(hint);
        }
        o.fields.find_id(id, self.jit_ctx.field_hint_base)
    }

    pub(crate) fn get_field_hint(&self, handle: u32, id: u32, hint: u32) -> (Value, u32) {
        // ⭐**THE READ FUNNEL USED TO MAKE A CALL PER FIELD READ TO LEARN A CONSTANT.** Both arms
        // below reached the pinned read-flip through `pinned_read_field`, a large `#[cold]`
        // function whose first statement was a `OnceLock` acquire pair. On the default path that
        // is a non-inlined call, a barrier LLVM cannot hoist out of the interpreter's dispatch
        // loop, and two loads — per getfield. One relaxed byte, read once here, decides it in a
        // register test, and the call is not made at all.
        let gw = heap_gates();
        if let Some(o) = self.get(handle) {
            // ⭐**THE HOTTEST OF THE FIVE, AND IT DERIVED THE BLOCK THREE TIMES TO READ ONE
            // SLOT.** `(o.fields.ids(), o.fields.cells(), o.fields.kinds())` is three slice
            // constructions: `len` loaded three times, `cap` twice, two `8 + k·cap` offsets
            // recomputed — for one allocation whose header cannot change under `&self`. And
            // then `hi < ids.len()` proved the index in range and the three following indexings
            // (`ids[hi]`, `cells[hi]`, `kinds[hi]`) each re-proved it, two of them behind a
            // panic landing pad. ONE header read, ONE bound test, on every interpreted getfield.
            let bv = BlobView::of(&o.fields);
            let fast = hg_derive(gw);
            let hi = hint as usize;
            // The per-site hint's verify. Identical predicate either way: `bv.n` IS
            // `ids.len()`, and `bv.id_at(hi)` IS `ids[hi]`, guarded by the same `hi < n`.
            let hit = if fast {
                // SAFETY: `hi < bv.n` is tested first and short-circuits.
                hi < bv.n && (unsafe { bv.id_at(hi) }) == id
            } else {
                let ids = o.fields.ids();
                hi < ids.len() && ids[hi] == id
            };
            if hit {
                // B4 read-flip (default OFF): serve the interp getfield funnel from
                // the pinned mirror; the returned INDEX stays the FieldBlob index
                // (the tlcache hint contract is untouched).
                #[cfg(feature = "next-gen-gc")]
                if hg_pread(gw) {
                    if let Some(v) = self.pinned_read_field(gw, handle, o.class.as_ref(), id) {
                        return (v, hint);
                    }
                }
                return (self.decode_slot(o, bv, fast, hi), hint);
            }
            // The caller's per-site hint missed; before walking, try the GLOBAL `fid`-bucketed
            // hint the compiled path keeps warm. A cold site on a hot field (the common shape
            // after a deopt or a first crossing) now costs a load, not a walk of every declared
            // field. O(fields-per-object) -> O(1) + verify.
            if let Some(i) = o.fields.find_id(id, self.jit_ctx.field_hint_base) {
                let i = i as usize;
                #[cfg(feature = "next-gen-gc")]
                if hg_pread(gw) {
                    if let Some(v) = self.pinned_read_field(gw, handle, o.class.as_ref(), id) {
                        return (v, i as u32);
                    }
                }
                // `find_id` proved `i < ids.len()`; the two reads below no longer re-prove it.
                return (self.decode_slot(o, bv, fast, i), i as u32);
            }
        }
        (Value::Null, hint)
    }

    /// Write field `id`, trying `hint` slot first. Returns the actual slot.
    pub(crate) fn put_field_hint(&self, handle: u32, id: u32, value: Value, hint: u32) -> u32 {
        let (cell, kind) = self.encode_field(&value);
        // Hint-table base, read before the exclusive borrow of the object slot.
        let hb = self.jit_ctx.field_hint_base;
        // ⭐**ONE WORD FOR THE PUTFIELD FUNNEL — IT WAS TWO ACQUIRE PAIRS AND AN `Arc` CLONE.**
        // This is the write half of both the interpreter's `putfield` and the compiled-to-host
        // putfield callback, so its per-operation cost is paid on every field store the engine
        // makes. It ended in `pinned_mirror_store` (a `pinned_mirror_enabled()` acquire pair) and
        // `shadow_enabled()` (another), and — the more expensive half — it cloned the object's
        // class `Arc<str>` UNCONDITIONALLY, two atomic read-modify-writes, purely so the mirror
        // publish would have a `&str` after the exclusive borrow of the object slot ended. The
        // mirror is OFF on every product boot, so all of that served nothing.
        //
        // The clone cannot be replaced by a borrow: `sync_obj_desc_incremental` takes `&mut self`
        // between the slot borrow and the publish, so no borrow of `o.class` can be proven to
        // live that far. What it CAN be is conditional on the gate that consumes it.
        // Read unconditionally now — the same word also carries `hg_derive`, the one-derivation
        // block view this funnel needs before its hint test. Still one relaxed load.
        let gw = heap_gates();
        let fast = hg_derive(gw);
        #[cfg(feature = "next-gen-gc")]
        let mirror_on = hg_mirror(gw);
        let result;
        let mut pushed = false;
        #[cfg(feature = "next-gen-gc")]
        let mut cls: Option<Arc<str>> = None;
        if let Some(o) = self.objects.write().unwrap_or_else(|e| e.into_inner()).get_mut(handle as usize).and_then(ObjectSlot::get_mut) {
            #[cfg(feature = "next-gen-gc")]
            if mirror_on {
                cls = Some(o.class.clone());
            }
            // ⭐**FOUR HEADER READS AND FOUR TESTS OF ONE FACT, TO STORE ONE SLOT.** The hit arm
            // read `len` in the explicit test, again inside `ids()` (plus `cap`), again inside
            // `cells_mut()`, and again inside `kinds_mut()` (plus `cap`) — and each of
            // `ids[hi]`, `cells_mut()[hi]`, `kinds_mut()[hi]` re-proved the `hi < len` the
            // explicit test had just established, three panic landing pads deep. This is the
            // write half of BOTH the interpreter's putfield and the compiled-to-host putfield
            // callback, so it is paid on every field store the engine makes. One header read,
            // one bound test. Taken before `find_id` (read-only) and never read after a `push`.
            let bv = BlobView::of(&o.fields);
            let hi = hint as usize;
            // Identical predicate: `bv.n` IS `o.fields.len()`, `bv.id_at(hi)` IS `ids()[hi]`.
            let hit = if fast {
                // SAFETY: `hi < bv.n` is tested first and short-circuits.
                hi < bv.n && (unsafe { bv.id_at(hi) }) == id
            } else {
                hi < o.fields.len() && o.fields.ids()[hi] == id
            };
            if hit {
                Self::store_slot(o, bv, fast, hi, cell, kind);
                result = hint;
            } else if let Some(i) = o.fields.find_id(id, hb) {
                // Site hint missed; the global `fid`-bucketed hint answers before the walk.
                // `find_id` proved `i < len`; the store no longer re-proves it.
                let i = i as usize;
                Self::store_slot(o, bv, fast, i, cell, kind);
                result = i as u32;
            } else {
                o.fields.push(id, cell, kind);
                result = (o.fields.len() - 1) as u32;
                pushed = true;
            }
        } else {
            return hint;
        }
        if pushed {
            self.bytes.fetch_add(13, std::sync::atomic::Ordering::Relaxed);
            // Incremental: the single pushed field is mirrored by
            // pinned_mirror_store below — skip the whole-object re-encode.
            self.sync_obj_desc_incremental(handle);
        }
        #[cfg(feature = "next-gen-gc")]
        if let Some(c) = cls {
            self.pinned_mirror_store(handle, &c, id, &value);
        }
        // Second consumer of the one word read at the top; this was the funnel's other acquire.
        #[cfg(feature = "next-gen-gc")]
        if hg_shadow(gw) && matches!(value, Value::Ref(_) | Value::Array(_) | Value::Obj(_)) {
            // A `Value::Obj` (raw arena pointer) stored into a handle object is the
            // SAME old→young edge as a Ref/Array handle — the card barrier must
            // record it or the moving collector can miss the arena object as a root.
            crate::nextgen::shadow().card_mark(handle);
        }
        result
    }
}
