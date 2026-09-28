//! **THE PER-SITE CLOSURE SHAPE, SO A MINT CAN BE EMITTED INSTEAD OF CALLED.**
//!
//! `RAVA_DIAG=indy` on a datapack run: **2,537,018 LambdaMetafactory assemblies**, of which only
//! 6,188 capture nothing. Every one of the rest is a callback into Rust that pops its captures and
//! bumps an arena object — while `emit_inline_concat` already shows the same allocation done from
//! emitted code for the StringConcatFactory arm.
//!
//! What an emitter needs is the site's `ArenaLambdaShape`: the capture-signature class id, the
//! interned impl-site index, the MethodHandle kind, and the `$fi` cell. All four are per-SITE
//! constants — the file that computes them says so ("a function of the CALL SITE alone") — but
//! none exists until the site has run once, because they come from resolving the bootstrap.
//!
//! ⭐So the same discipline the arm work used today: **the table is filled by the call it replaces,
//! the emitter bakes only its ADDRESS, and the emitted guard refuses while the entry is zero.** A
//! method compiled before its site first executed therefore still picks the shape up, which is the
//! bootstrap trap that kept three inline arms at zero earlier in this session.
//!
//! Direct-mapped with the site key stored beside the value — the collision lesson the site-keyed
//! getter table paid an access violation for.
//!
//! This file also carries the **per-site indy RESULT cache** (`SITE_*` below), which serves the
//! other half of the indy family: a NON-capturing site is a `ConstantCallSite` whose object is
//! bound once, so a warm site needs no `rava_indy` / `invokedynamic_assemble` at all — one load
//! and a push.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// The HISTORIC table: 4096 direct-mapped rows, the mask `slot()` applies with HR-UNDESC off.
const N: usize = 4096;
/// The row store's real extent. With HR-UNDESC on, a site is assigned a row of its OWN by linear
/// probing from its hash (`claim_row`) instead of sharing `slot()` with every other site that
/// hashes there, so the store is wider than the historic mask. Off, only the first `N` rows are
/// ever addressed and the extra rows are untouched `.bss`.
const N_WIDE: usize = 8192;
/// How far `claim_row`/`find_row` walk from a site's home row before refusing. A refusal is
/// counted (`ROW_FULL`) and reported; it can only ever REJECT a site from the mint.
const PROBE: usize = 64;
#[allow(clippy::declare_interior_mutable_const)]
const Z: AtomicU64 = AtomicU64::new(0);

/// `(cid + 1) << 40 | (site_idx) << 8 | kind` — cid and site index are u32 in principle but a
/// process holds thousands of each, so 24 and 32 bits are ample and leave the low byte for the
/// MethodHandle kind. Zero means "nothing known about this site".
pub(crate) static VALS: [AtomicU64; N_WIDE] = [Z; N_WIDE];
pub(crate) static KEYS: [AtomicU64; N_WIDE] = [Z; N_WIDE];
/// The `$fi` cell — a per-site constant the mint writes into the last header slot. Kept in its own
/// table because it is a full 64-bit encoded cell.
pub(crate) static FI: [AtomicU64; N_WIDE] = [Z; N_WIDE];
/// Capture-slot kinds, one nibble per slot, up to `max_captures()` captures (14 with HR-INDY on,
/// the historic 8 off), with the shape's own arity in the top byte. The emitter needs the
/// nibbles to cross-check its own descriptor parse against the recorder's, and the arity because
/// it is NOT always the indy descriptor's argument count.
pub(crate) static KINDS: [AtomicU64; N_WIDE] = [Z; N_WIDE];
/// HR-UNDESC: which site OWNS a row (`key_for`, never 0). Claimed once by a `compare_exchange`
/// from 0, never released — so a probe that meets a 0 has proven the site absent. `KEYS` keeps
/// its historic meaning ("the row is PUBLISHED for this site"); this is the weaker "the row is
/// RESERVED for this site", which the emitter needs before any shape exists.
static OWNER: [AtomicU64; N_WIDE] = [Z; N_WIDE];
/// A row nothing ever writes. `entry_addrs` hands it to a site the store could not seat, so the
/// emitted guard reads `key == 0 != want` and refuses on every execution exactly as it would
/// have for a lost collision — the historic failure, now counted (`ROW_FULL`).
static SINK: [AtomicU64; 4] = [Z; 4];

#[inline]
fn mix(caller: usize, cp: u16) -> u64 {
    (caller as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (cp as u64).wrapping_mul(0x517C_C1B7)
}

/// The HISTORIC direct-mapped slot (HR-UNDESC off): a hash, and nothing resolves a collision.
#[inline]
fn slot(caller: usize, cp: u16) -> usize {
    ((mix(caller, cp) >> 19) as usize) & (N - 1)
}

/// HR-UNDESC home row: the same mix over the wide store.
#[inline]
fn home(caller: usize, cp: u16) -> usize {
    ((mix(caller, cp) >> 19) as usize) & (N_WIDE - 1)
}

#[inline]
pub(crate) fn key_for(caller: usize, cp: u16) -> u64 {
    ((caller as u64) ^ ((cp as u64) << 48)) | 1
}

/// ⭐**THE ROW A SITE OWNS — RESERVED ON FIRST ASK, NEVER SHARED, NEVER MOVED.**
///
/// The `[PC-INDY]` census counted **282,962 `undescribed` slow-arm refusals against 2,097,194
/// mints** — 12% of the mint's population leaving machine code because the row the emitted
/// guard reads has no shape in it. The recorder DOES run on the first compiled visit
/// (`jit_indy` → `opcodes::invokedynamic` → the Lambda arm's `record`), so a site that stays
/// undescribed after its first callback is one the table cannot converge on, and the historic
/// table had exactly one such site: **the loser of a direct-mapped collision.** `record` is
/// first-writer-wins on `VALS[slot]`, so of two sites hashing to one of 4096 rows the second is
/// refused on every execution for the life of the process — and the emitter had baked that
/// row's address, so nothing ever re-asked. With a few thousand live capturing sites the
/// birthday bound puts the losers in the hundreds, and every one of their mints is a callback.
///
/// Here each site is seated in a row of its own by linear probing from its home row, claimed
/// with a `compare_exchange` on `OWNER` (so two threads seating one site agree on the row, and
/// two sites can never agree on one). The emitter claims at emit time (`entry_addrs`), the
/// recorder claims at record time; whichever comes first fixes the row and the other finds it.
/// `find_row` is the read-only twin, for the censuses and the emit-time admission reads.
fn claim_row(caller: usize, cp: u16) -> Option<usize> {
    let key = key_for(caller, cp);
    let h = home(caller, cp);
    for j in 0..PROBE {
        let i = (h + j) & (N_WIDE - 1);
        match OWNER[i].compare_exchange(0, key, Relaxed, Relaxed) {
            Ok(_) => {
                ROWS_CLAIMED.fetch_add(1, Relaxed);
                return Some(i);
            }
            Err(o) if o == key => return Some(i),
            Err(_) => {}
        }
    }
    ROW_FULL.fetch_add(1, Relaxed);
    None
}

fn find_row(caller: usize, cp: u16) -> Option<usize> {
    let key = key_for(caller, cp);
    let h = home(caller, cp);
    for j in 0..PROBE {
        let i = (h + j) & (N_WIDE - 1);
        match OWNER[i].load(Relaxed) {
            0 => return None,
            o if o == key => return Some(i),
            _ => {}
        }
    }
    None
}

/// The row to READ for a site: the owned row (HR-UNDESC) or the historic direct-mapped slot.
#[inline]
fn row_for_read(caller: usize, cp: u16) -> Option<usize> {
    if undesc_enabled() {
        find_row(caller, cp)
    } else {
        Some(slot(caller, cp))
    }
}

/// Addresses the emitter bakes: (key cell, value cell, fi cell, kinds cell).
pub(crate) fn entry_addrs(caller: usize, cp: u16) -> (i64, i64, i64, i64) {
    if undesc_enabled() {
        return match claim_row(caller, cp) {
            Some(i) => row_addrs(i),
            None => (
                (&SINK[0] as *const AtomicU64) as i64,
                (&SINK[1] as *const AtomicU64) as i64,
                (&SINK[2] as *const AtomicU64) as i64,
                (&SINK[3] as *const AtomicU64) as i64,
            ),
        };
    }
    row_addrs(slot(caller, cp))
}

#[inline]
fn row_addrs(i: usize) -> (i64, i64, i64, i64) {
    (
        (&KEYS[i] as *const AtomicU64) as i64,
        (&VALS[i] as *const AtomicU64) as i64,
        (&FI[i] as *const AtomicU64) as i64,
        (&KINDS[i] as *const AtomicU64) as i64,
    )
}

/// The most capture nibbles one `KINDS` word can hold beside its arity byte: 14 × 4 = 56 bits
/// under the byte at 56..64. The historic bound was 8 — half the word left empty — and on the
/// bootstrap bench the two heaviest array-capturing sites (`BitSetDiscreteVoxelShape.lambda$join$2`
/// at 10 captures and `lambda$join$1` at 12) fell outside it, so their shape was never even
/// RECORDED and the mint could not have served them from any method. 14 is the word's own
/// bound, not a guess; `hr_enabled()` off restores 8 exactly.
pub(crate) fn max_captures() -> usize {
    if hr_enabled() {
        14
    } else {
        8
    }
}

/// Record a site's shape, from the assembly that just ran. First writer wins.
pub(crate) fn record(caller: usize, cp: u16, cid: u32, site_idx: u32, kind: u8, fi: i64, kinds: &[u8]) {
    let undesc = undesc_enabled();
    if kinds.len() > max_captures() || cid as u64 >= (1 << 24) {
        // HR-UNDESC census only: a shape the row cannot hold is a site the mint can never
        // serve, and until now it was refused silently.
        if undesc {
            REC_REFUSE[if kinds.len() > max_captures() { 0 } else { 1 }].fetch_add(1, Relaxed);
        }
        return;
    }
    // HR-UNDESC: the site's OWN row (see `claim_row`). A store that cannot seat it refuses here
    // and is counted; the emitted guard for such a site reads `SINK` and refuses too.
    let i = if undesc {
        match claim_row(caller, cp) {
            Some(i) => i,
            None => return,
        }
    } else {
        slot(caller, cp)
    };
    let mut packed_kinds: u64 = 0;
    for (n, &k) in kinds.iter().enumerate() {
        packed_kinds |= ((k & 0xF) as u64) << (n * 4);
    }
    // ⭐**THE SHAPE'S OWN ARITY, in the top byte, because it is NOT always the indy descriptor's
    // argument count.** A bound method reference can carry a capture the descriptor does not
    // list, and trusting the descriptor minted a closure with the wrong `nfields` — the object
    // then decoded as something else entirely (`array.get` on a closure) at arity 3 while arity
    // 1 and 2 happened to agree. The emitted guard compares this against its baked count and
    // refuses on any mismatch, so the arm can never again be wrong about a shape it did not see.
    packed_kinds |= (kinds.len() as u64) << 56;
    let v = ((cid as u64 + 1) << 40) | ((site_idx as u64 & 0xFFFF_FFFF) << 8) | kind as u64;
    // Values first, key last: a reader that sees its own key sees a fully published row.
    if VALS[i].compare_exchange(0, v, Relaxed, Relaxed).is_ok() {
        FI[i].store(fi as u64, Relaxed);
        KINDS[i].store(packed_kinds, Relaxed);
        KEYS[i].store(key_for(caller, cp), Relaxed);
        RECORDED.fetch_add(1, Relaxed);
    }
}

pub(crate) static RECORDED: AtomicU64 = AtomicU64::new(0);
/// Mints answered from emitted code.
pub(crate) static MINTED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn recorded() -> u64 {
    RECORDED.load(Relaxed)
}
pub(crate) fn minted() -> u64 {
    MINTED.load(Relaxed)
}

/// **DEFAULT ON — kill switch `RAVA_NO_INLINE_LAMBDA=1`.**
///
/// It shipped opt-in while the arm was wrong at three captures, and the note here recorded that
/// as an unexplained arity limit. It was not an arity limit. The mint stored the JIT operand cell
/// STRAIGHT into an arena capture slot, and the two encodings are not the same one:
///
///   * JIT operand cell (`jit::abi::pack_local`/`box_ref`): high-word tags —
///     `0` null, raw arena pointer (`cell >> 40 == 0`), `h|REF_TAG` (`1<<40`),
///     `h|ARRAY_TAG` (`1<<42`), `idx|BOX_TAG` (`1<<41`, TRANSIENT — unwinds with the frame).
///   * arena capture cell (`opcodes::encode_arena_cell`): LOW-2-BIT tags —
///     `0b00` raw arena pointer / null, `0b01` `(h<<2)|1` heap handle, `0b10` `(h<<2)|2` array,
///     `0b11` box index.
///
/// They agree on exactly one form — the raw arena pointer — which is why one- and two-capture
/// sites whose captures happened to be arena objects minted 4,544 correct closures, and why the
/// first site with a HEAP-HANDLE capture died. `h|REF_TAG` read as an arena cell is
/// `(h|1<<40) & 3`: `0` when `h` is 4-aligned, and `is_arena_obj_addr` then rejects the address
/// ⇒ **`Value::Null` — the null local**; `1` when `h ≡ 1 (mod 4)` ⇒ `Value::Ref(cell >> 2)`, a
/// WRONG live handle ⇒ **the InternalError the spill probe saw**. Both recorded symptoms, from
/// one missing conversion. `emit_inline_lambda` now converts the tag or refuses.
pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_INLINE_LAMBDA").is_none())
}

/// The recorded shape for a site, if this exact site has one: `(cid, site_idx, kind, arity)`.
///
/// ⭐**Read at EMIT time for ADMISSION, never for LAYOUT.** The emitted guard re-reads the same
/// row at runtime and the layout comes from there, because a method usually compiles before some
/// of its indy sites have ever run (requiring the entry at emit time cost 20x coverage). What the
/// emit-time read buys is a HARD REFUSAL: when the row already exists and disagrees with the
/// descriptor — a different arity, a MethodHandle kind outside the five `LambdaMetafactory`
/// accepts, a capture nibble this arm does not write — the arm is not emitted at all, instead of
/// being emitted and then turned away by a guard on every execution.
pub(crate) fn shape_at(caller: usize, cp: u16) -> Option<(u32, u32, u8, usize)> {
    let i = row_for_read(caller, cp)?;
    if KEYS[i].load(Relaxed) != key_for(caller, cp) {
        return None;
    }
    let v = VALS[i].load(Relaxed);
    if v == 0 {
        return None;
    }
    let arity = (KINDS[i].load(Relaxed) >> 56) as usize;
    Some((
        ((v >> 40) - 1) as u32,
        ((v >> 8) & 0xFFFF_FFFF) as u32,
        (v & 0xFF) as u8,
        arity,
    ))
}

/// The packed capture-kind nibbles recorded for a site (arity in the top byte), if the row is
/// this site's. Read at emit time so the arm can cross-check the recorder's descriptor parse
/// (`concat_param_types`) against its own — a disagreement means the two sides do not describe
/// the same capture list, and the arm refuses rather than laying cells from the losing parse.
pub(crate) fn packed_kinds_at(caller: usize, cp: u16) -> Option<u64> {
    let i = row_for_read(caller, cp)?;
    if KEYS[i].load(Relaxed) != key_for(caller, cp) {
        return None;
    }
    if VALS[i].load(Relaxed) == 0 {
        return None;
    }
    Some(KINDS[i].load(Relaxed))
}

/// The `$fi` cell recorded for a site.
#[allow(dead_code)]
pub(crate) fn fi_at(caller: usize, cp: u16) -> i64 {
    row_for_read(caller, cp).map(|i| FI[i].load(Relaxed) as i64).unwrap_or(0)
}

// ===========================================================================================
// HR-INDY — the site-cursor mint arm, and the census of WHY a closure was not minted inline.
// ===========================================================================================

/// **DEFAULT ON — kill switch `RAVA_NO_HR_INDY=1`** (also off under `RAVA_NO_EMIT_MORE`).
///
/// Gates, together: (1) the SITE-CURSOR mint arm — `emit_inline_lambda_sitecursor`, which
/// reaches an `invokedynamic` in a method that has NO hoisted TLAB cursor (`body.rs` creates one
/// only for a method with an arena `new` or a concat shape; a method whose only allocation is
/// a closure never had one, so the mint was never even attempted there); (2) the per-reason
/// refusal census below, emitted as plain load/add/store cells on the mint's SLOW arm and read
/// by the periodic profiler dump; (3) the `rava_indy` callback census (`picprobe`-gated);
/// (4) the shape table's capture bound, 14 instead of 8 (`max_captures`). Off, the mint emits
/// byte-for-byte what it emitted before this round, the recorder refuses past 8 as it did, and
/// the counters stay at zero.
pub(crate) fn hr_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_HR_INDY").is_none())
}

/// EMIT-TIME refusals, one per compiled SITE (never on the per-call path, so a relaxed add).
///
///   * `no_cursor`     — a POPULATION count, not a refusal: capturing sites compiled in a method
///                       with no hoisted TLAB cursor — the sites the mint never reached before
///                       this round. Counted at the `invoke_dynamic` arm before the site-cursor
///                       entry is tried; `sites armed: sitecursor=` says how many of them it took.
///   * `arity_bound`   — more captures than the 8 nibbles the shape row packs (`RAVA_LM_MAXN`).
///   * `stack_short`   — the emitter's operand stack holds fewer values than the descriptor
///                       declares (a malformed site; the callback handles it).
///   * `desc_parse`    — the descriptor's parameter list did not parse into capture kinds.
///   * `operand_type`  — a capture's JIT operand type disagrees with its descriptor slot (a
///                       primitive headed for a reference slot must be boxed; emitted code
///                       cannot).
///   * `recorded_disagrees` — the site's recorded shape (arity / MethodHandle kind / a capture
///                       nibble) disagrees with the descriptor.
pub(crate) const EMIT_REASONS: [&str; 6] =
    ["no_cursor", "arity_bound", "stack_short", "desc_parse", "operand_type", "recorded_disagrees"];
pub(crate) static EMIT_REFUSE: [AtomicU64; 6] = [Z; 6];
/// Sites where the mint arm WAS emitted: `[hoisted cursor, site cursor]`.
pub(crate) static EMIT_ARMED: [AtomicU64; 2] = [Z; 2];

#[inline]
pub(crate) fn note_emit_refuse(reason: usize) {
    EMIT_REFUSE[reason.min(EMIT_REASONS.len() - 1)].fetch_add(1, Relaxed);
}

/// RUN-TIME refusals, one per EXECUTION that took the mint's slow arm, bumped from emitted code
/// as a plain load/add/store on that arm only — a refusal already pays the whole `rava_indy`
/// crossing, so the cell is noise there and absent from the fast arm entirely.
///
///   * `undescribed`   — the site's shape row is not published yet (key mismatch or zero
///                       value): the very call this arm falls back to is what publishes it.
///   * `kind_or_arity` — the row is present but its MethodHandle kind is outside 5..=9 or its
///                       arity differs from the descriptor's argument count.
///   * `capture_tag`   — a reference capture carries a cell this arm cannot re-encode
///                       (`BOX_TAG` / `ABOX_TAG`) or a misaligned untagged cell.
///   * `tlab_full`     — the bump did not fit; the callback collects.
///   * `forced_slow`   — `RAVA_LM_FORCE_SLOW` bisection handle.
pub(crate) const RT_REASONS: [&str; 5] =
    ["undescribed", "kind_or_arity", "capture_tag", "tlab_full", "forced_slow"];
pub(crate) static RT_REFUSE: [AtomicU64; 5] = [Z; 5];
/// Mints answered from emitted code through the SITE-CURSOR arm (a subset of `MINTED`).
pub(crate) static MINTED_SITECURSOR: AtomicU64 = AtomicU64::new(0);

/// Base address of `RT_REFUSE`, baked by the emitter; the arm indexes it by reason * 8.
pub(crate) fn rt_refuse_base() -> i64 {
    RT_REFUSE.as_ptr() as i64
}

/// What reaches the `rava_indy` CALLBACK, by the shape the mint would have seen — the
/// complement of the served population. `picprobe`-gated (one relaxed add per callback).
///
///   * `noncapturing`   — zero dynamic arguments (the site-cache's population, not the mint's).
///   * `cap_mintable`   — a `LambdaMetafactory` site whose every reference capture is a null,
///                       an 8-aligned raw arena pointer, `h|REF_TAG` or `h|ARRAY_TAG`: the mint
///                       could have laid every cell, so this count IS the remaining prize.
///   * `cap_boxidx`     — at least one capture is a `BOX_TAG` cell (transient frame box table).
///   * `cap_other_tag`  — at least one capture carries another high-word form (`ABOX_TAG`, a
///                       misaligned untagged cell).
///   * `not_metafactory` — concat / record / switch bootstraps: never a closure.
pub(crate) const CB_KINDS: [&str; 5] =
    ["noncapturing", "cap_mintable", "cap_boxidx", "cap_other_tag", "not_metafactory"];
pub(crate) static CB_CENSUS: [AtomicU64; 5] = [Z; 5];

/// Classify one callback for `CB_CENSUS`. Reads the bootstrap the instruction names (JVMS
/// 4.7.23) and the high-word tag of each cell — the instruction's own operand and the cells'
/// own encoding, nothing else.
pub(crate) fn note_callback(class: &crate::class_loader::LoadedClass, cp: u16, cells: &[i64]) {
    let metafactory = bootstrap_owner(class, cp) == Some(BSM_METAFACTORY);
    let k = if !metafactory {
        4
    } else if cells.is_empty() {
        0
    } else {
        // A primitive capture's raw bits can look like any tag; only a REFERENCE capture is
        // tag-tested, and the descriptor says which slots those are.
        let desc = class.file.pool.invoke_dynamic(cp).map(|(_, _, d)| d).unwrap_or("");
        let types = crate::opcodes::concat_param_types(desc);
        let mut worst = 1;
        for (i, &c) in cells.iter().enumerate() {
            if !matches!(types.get(i), Some(b'L') | Some(b'[')) {
                continue;
            }
            let hi = (c as u64) >> 40;
            let bad = match hi {
                0 => {
                    if c & 7 != 0 {
                        3
                    } else {
                        continue;
                    }
                }
                1 | 4 => continue,
                2 => 2,
                _ => 3,
            };
            worst = worst.max(bad);
        }
        worst
    };
    CB_CENSUS[k].fetch_add(1, Relaxed);
}

/// The bootstrap-method OWNER the instruction names (JVMS 4.7.23): the site's own linkage
/// operand, which is what `opcodes::invokedynamic` dispatches its `Plan` on. Reading it here is
/// reading the instruction, not routing by a workload class name.
fn bootstrap_owner(class: &crate::class_loader::LoadedClass, cp: u16) -> Option<&str> {
    class
        .file
        .pool
        .invoke_dynamic(cp)
        .and_then(|(bsm, _, _)| class.file.bootstrap_methods.get(bsm as usize))
        .and_then(|bm| class.file.pool.method_handle(bm.handle_index))
        .map(|h| h.owner)
}
const BSM_METAFACTORY: &str = "java/lang/invoke/LambdaMetafactory";
const BSM_CONCAT: &str = "java/lang/invoke/StringConcatFactory";

// ===========================================================================================
// HR-UNDESC — why a site stays undescribed after its first callback, and the two structural
// causes that could never converge. Kill switch `RAVA_NO_HR_UNDESC=1`.
// ===========================================================================================

/// **DEFAULT ON — kill switch `RAVA_NO_HR_UNDESC=1`** (also off under `RAVA_NO_EMIT_MORE`).
///
/// The `[PC-INDY]` census: `undescribed=282,962` slow-arm refusals — 12% of all mints leave
/// machine code because the site's shape word is zero when the compiled probe reads it. The
/// recorder runs on the compiled callback too (`jit_indy` → `opcodes::invokedynamic` → the
/// capturing Lambda arm → `record`, once per thread and site), so a first-visit refusal is
/// inherent and bounded by the site count. What is NOT bounded is a site that stays
/// undescribed after that callback, and the tree had two of those, both structural:
///
///   1. **A lost direct-mapped collision.** 4096 rows, first-writer-wins: the second site to
///      hash into a row is refused on every execution for the life of the process, and the
///      emitter baked that row's address so nothing ever re-asked. Cured by `claim_row`: every
///      site owns a row, reserved on first ask by emitter or recorder alike.
///   2. **A non-`LambdaMetafactory` site carrying the mint arm.** `invoke_dynamic` tried the
///      mint on every capturing indy whose descriptor parsed — a `StringConcatFactory` site the
///      concat arm did not take, an `ObjectMethods` record site, a `SwitchBootstraps` site —
///      and the recorder, which lives in the Lambda arm alone, could never describe one. Every
///      execution paid the guard and the callback. Cured by `mint_admits_site`, read at emit
///      time from the instruction's bootstrap operand: the arm is not emitted there at all.
///
/// Gates, together: the owned-row store (`claim_row`/`find_row` in place of `slot`), the
/// emit-time admission, the recorder's refusal census (`REC_REFUSE`), and the callback census
/// (`CB_UNDESC`, `CB_PUBLISHED`). Off, `slot()` is the historic direct-mapped index over the
/// historic 4096 rows, `entry_addrs`/`record`/`shape_at` read and write exactly what they did,
/// `mint_admits_site` admits everything, and every counter here stays at zero.
pub(crate) fn undesc_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // MEASURED OPT-IN on the bootstrap bench (3 pairs, B = disabled): -3.00/-1.32/-2.04% min,
    // -1.42/-0.56/-0.99% median, one DISJOINT -- disabling it is FASTER. The mechanism is right (the
    // direct-mapped table did lose collisions forever) but the replacement pays more per probe than
    // the lost mints cost on this population. `RAVA_FAST_HR_UNDESC=1` arms it.
    *ON.get_or_init(|| !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_FAST_HR_UNDESC").is_some())
}

/// Rows seated by `claim_row` (one per distinct site the emitter or recorder asked about) and
/// sites the store could not seat within `PROBE` of their home (refused, `SINK`-guarded).
pub(crate) static ROWS_CLAIMED: AtomicU64 = AtomicU64::new(0);
pub(crate) static ROW_FULL: AtomicU64 = AtomicU64::new(0);
/// `record` refusals, once per (thread, site): `[arity > max_captures, cid >= 2^24]`.
pub(crate) static REC_REFUSE: [AtomicU64; 2] = [Z; 2];
/// Emit-time admission refusals of `mint_admits_site`, once per compiled site:
/// `[StringConcatFactory, other non-metafactory bootstrap (records / switch / undecodable)]`.
pub(crate) const NONLAMBDA_KINDS: [&str; 2] = ["concat", "record_switch_other"];
pub(crate) static EMIT_NONLAMBDA: [AtomicU64; 2] = [Z; 2];
/// Callbacks that arrived at an undescribed capturing `LambdaMetafactory` site and LEFT it
/// described — the first visit, the one publication the design requires.
pub(crate) static CB_PUBLISHED: AtomicU64 = AtomicU64::new(0);
/// Callbacks that arrived at an undescribed capturing `LambdaMetafactory` site and left it
/// STILL undescribed — the repeat population, by the cause that kept the recorder out:
///
///   * `worker`     — a GIL-free worker mints handle lambdas and never reaches the arena arm
///                    that records (`invokedynamic_assemble`'s `!gil::is_worker()` guard).
///   * `row_full`   — the store could not seat the site (`ROW_FULL`); its guard reads `SINK`.
///   * `arena_off`  — `RAVA_NO_ARENA_LAMBDA`: no arena shape exists to record.
///   * `rec_refused`— the shape's arity or class id is outside what a row can hold.
///   * `other`      — none of the above named it (a fault before the arm, a dedup hit that
///                    pre-empted the first mint, or a cause this census has not learned).
pub(crate) const UNDESC_CAUSES: [&str; 5] = ["worker", "row_full", "arena_off", "rec_refused", "other"];
pub(crate) static CB_UNDESC: [AtomicU64; 5] = [Z; 5];

/// Is the site's shape row published? The read the emitted guard makes, made from Rust.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn described(caller: usize, cp: u16) -> bool {
    match find_row(caller, cp) {
        Some(i) => KEYS[i].load(Relaxed) == key_for(caller, cp) && VALS[i].load(Relaxed) != 0,
        None => false,
    }
}

/// The census token `rava_indy` takes BEFORE it runs the site: `Some(())` when this callback
/// is at an undescribed capturing `LambdaMetafactory` site — the mint's own population, the
/// only one the slow arm's `undescribed` count can name — else `None` and nothing is counted.
/// Two loads and a bootstrap-operand read on a path that is already a full host crossing; no
/// atomic RMW here at all.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn undesc_before(class: &crate::class_loader::LoadedClass, cp: u16, ncells: usize) -> Option<()> {
    if !undesc_enabled() || ncells == 0 {
        return None;
    }
    let caller = class as *const crate::class_loader::LoadedClass as usize;
    if described(caller, cp) {
        return None;
    }
    (bootstrap_owner(class, cp) == Some(BSM_METAFACTORY)).then_some(())
}

/// The census's second half, AFTER the site ran: published on this visit, or still not — and
/// then which structural cause. Counted only on the arm `undesc_before` admitted, so the RMW
/// is bounded by the population the round exists to remove.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn undesc_after(class: &crate::class_loader::LoadedClass, cp: u16, ncells: usize) {
    let caller = class as *const crate::class_loader::LoadedClass as usize;
    if described(caller, cp) {
        CB_PUBLISHED.fetch_add(1, Relaxed);
        return;
    }
    let cause = if crate::gil::is_worker() {
        0
    } else if find_row(caller, cp).is_none() {
        // The recorder's own `claim_row` would have seated the site; no row after it ran
        // means the store refused it (`ROW_FULL`).
        1
    } else if !crate::nextgen::arena_lambda_enabled() {
        2
    } else if ncells > max_captures() {
        3
    } else {
        4
    };
    CB_UNDESC[cause].fetch_add(1, Relaxed);
}

/// EMIT-TIME ADMISSION for the mint arm: is this a `LambdaMetafactory` site at all? Read from
/// the instruction's own bootstrap operand (`bootstrap_owner`), never from a workload name. A
/// site whose bootstrap is anything else has no closure shape to record — the recorder lives in
/// the Lambda arm — so a mint arm planted there refuses `undescribed` on every execution
/// forever. Refusing at emit costs nothing at run time; admitting cost the guard plus the
/// callback per execution. Off, admits everything: the historic arm.
///
/// `caller` is `env.class_ptr`, the loader-owned class the emitter is compiling; a null one
/// (`guarded_inline` treats 0 as "no class") is refused rather than dereferenced.
pub(crate) fn mint_admits_site(caller: usize, cp: u16) -> bool {
    if !undesc_enabled() {
        return true;
    }
    if caller == 0 {
        EMIT_NONLAMBDA[1].fetch_add(1, Relaxed);
        return false;
    }
    // SAFETY: `env.class_ptr` is a live loader-owned `LoadedClass` for the whole compile — the
    // same pointer `rava_indy` dereferences on every callback from the emitted body.
    let class: &crate::class_loader::LoadedClass = unsafe { &*(caller as *const crate::class_loader::LoadedClass) };
    match bootstrap_owner(class, cp) {
        Some(BSM_METAFACTORY) => true,
        Some(BSM_CONCAT) => {
            EMIT_NONLAMBDA[0].fetch_add(1, Relaxed);
            false
        }
        _ => {
            EMIT_NONLAMBDA[1].fetch_add(1, Relaxed);
            false
        }
    }
}

/// The `[PC-INDY]` line for the periodic profiler dump. Empty until the mint has been asked
/// anything at all, so a workload without a compiled indy site prints nothing.
pub fn report() -> String {
    let minted = MINTED.load(Relaxed);
    let minted_sc = MINTED_SITECURSOR.load(Relaxed);
    let armed: Vec<u64> = EMIT_ARMED.iter().map(|c| c.load(Relaxed)).collect();
    let er: Vec<u64> = EMIT_REFUSE.iter().map(|c| c.load(Relaxed)).collect();
    let rr: Vec<u64> = RT_REFUSE.iter().map(|c| c.load(Relaxed)).collect();
    let cb: Vec<u64> = CB_CENSUS.iter().map(|c| c.load(Relaxed)).collect();
    let claimed = ROWS_CLAIMED.load(Relaxed);
    let full = ROW_FULL.load(Relaxed);
    let rec: Vec<u64> = REC_REFUSE.iter().map(|c| c.load(Relaxed)).collect();
    let nl: Vec<u64> = EMIT_NONLAMBDA.iter().map(|c| c.load(Relaxed)).collect();
    let published = CB_PUBLISHED.load(Relaxed);
    let und: Vec<u64> = CB_UNDESC.iter().map(|c| c.load(Relaxed)).collect();
    let touched = minted
        + armed.iter().sum::<u64>()
        + er.iter().sum::<u64>()
        + rr.iter().sum::<u64>()
        + cb.iter().sum::<u64>()
        + claimed
        + nl.iter().sum::<u64>();
    if touched == 0 {
        return String::new();
    }
    let mut out = format!(
        "mint served={} (hoisted={} sitecursor={}) sites_recorded={} | sites armed: hoisted={} sitecursor={}",
        minted,
        minted.saturating_sub(minted_sc),
        minted_sc,
        RECORDED.load(Relaxed),
        armed[0],
        armed[1]
    );
    out.push_str(" | emit refusals:");
    for (n, s) in er.iter().zip(EMIT_REASONS) {
        out.push_str(&format!(" {s}={n}"));
    }
    out.push_str(" | slow-arm refusals:");
    for (n, s) in rr.iter().zip(RT_REASONS) {
        out.push_str(&format!(" {s}={n}"));
    }
    if cb.iter().sum::<u64>() > 0 {
        out.push_str(" | rava_indy callbacks:");
        for (n, s) in cb.iter().zip(CB_KINDS) {
            out.push_str(&format!(" {s}={n}"));
        }
    }
    // HR-UNDESC (`RAVA_NO_HR_UNDESC=1` leaves every count at zero and the line unprinted):
    // rows seated / refused, the recorder's own refusals, the non-lambda sites the mint arm no
    // longer carries, and the callback's undescribed census split first-visit vs repeat-by-cause.
    if undesc_enabled() {
        out.push_str(&format!(
            "\n[PC-INDY]   undesc: rows claimed={claimed} store_full={full} | record refused: arity={} cid={} | mint arm not emitted (non-lambda bootstrap):",
            rec[0], rec[1]
        ));
        for (n, s) in nl.iter().zip(NONLAMBDA_KINDS) {
            out.push_str(&format!(" {s}={n}"));
        }
        out.push_str(&format!(
            "\n[PC-INDY]   undesc callbacks at capturing lambda sites: first-visit(published by this callback)={published} | repeat(still undescribed):"
        ));
        for (n, s) in und.iter().zip(UNDESC_CAUSES) {
            out.push_str(&format!(" {s}={n}"));
        }
    }
    out
}

// ===========================================================================================
// PER-SITE INDY RESULT CACHE — the non-capturing half of the indy family.
// ===========================================================================================

/// The result cell a warm non-capturing indy site yields, and the site key beside it.
///
/// **Why a value cache and not a target cache.** A non-capturing `LambdaMetafactory` site is a
/// `ConstantCallSite`: `opcodes::invokedynamic` links it once and every later execution returns
/// the SAME object out of `indy_singletons` / `arena_indy_singletons` — the top of that function
/// is literally a map lookup and a push. From compiled code that costs a whole `rava_indy`
/// crossing (worker sanction, `vm_ref`, descriptor `to_string`, a synthetic `Frame`, the
/// thread-local plan cache) to produce a constant. Caching the RESULT removes the call entirely
/// rather than making it cheaper.
///
/// Published value-then-key, read key-then-value with BOTH required, so a reader can only ever
/// see a complete row or refuse. A slot collision between two sites is resolved by the key: the
/// loser re-publishes and both stay correct.
pub(crate) static SITE_VALS: [AtomicU64; N] = [Z; N];
pub(crate) static SITE_KEYS: [AtomicU64; N] = [Z; N];

/// Addresses the emitter bakes for the result cache: (key cell, value cell).
pub(crate) fn site_entry_addrs(caller: usize, cp: u16) -> (i64, i64) {
    let i = slot(caller, cp);
    (
        (&SITE_KEYS[i] as *const AtomicU64) as i64,
        (&SITE_VALS[i] as *const AtomicU64) as i64,
    )
}

/// Sites published into the result cache, and hits served from emitted code.
pub(crate) static SITE_PUBLISHED: AtomicU64 = AtomicU64::new(0);
pub(crate) static SITE_HITS: AtomicU64 = AtomicU64::new(0);

#[allow(dead_code)]
pub(crate) fn site_published() -> u64 {
    SITE_PUBLISHED.load(Relaxed)
}
#[allow(dead_code)]
pub(crate) fn site_hits() -> u64 {
    SITE_HITS.load(Relaxed)
}

/// **DEFAULT ON — kill switch `RAVA_NO_INDY_SITE_CACHE=1`.**
pub(crate) fn site_cache_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_INDY_SITE_CACHE").is_none())
}

/// Publish `res` as site `key`'s constant result — called from emitted code on the MISS arm, once
/// per site, immediately after the `rava_indy` it will replace.
///
/// ⭐**THE ADMISSION IS A PROPERTY OF THE CELL, NOT A BELIEF ABOUT THE SITE.** Only two cell forms
/// name an object that is immortal AND address-stable, and the emitter cannot test either of them
/// inline (`PINNED_BASE`/`PINNED_TOP` are private to `nextgen`), which is the whole reason this is
/// a call:
///
///   * a **pinned arena lambda** — `is_pinned` bounds it to the live pinned region, which never
///     moves and never frees, and `is_arena_lambda` proves from its class id that it is a closure
///     and not something else that happens to live there. This is what
///     `arena_lambda_singleton_enabled()` (default ON) mints for a non-capturing site.
///   * a **heap handle** (`cell >> 40 == 1`, `h|REF_TAG`) — the fallback the same site takes when
///     the pinned region is exhausted, and `Vm::indy_singletons` roots it for the process.
///
/// Everything else REFUSES and the site keeps calling: a `BOX_TAG` cell (`>> 40 == 2`) indexes the
/// TRANSIENT frame box table and is dangling the moment the compiled frame unwinds; a MOVING
/// arena string — what a zero-argument `StringConcatFactory` site returns — is a raw pointer that
/// looks exactly like an arena object and is neither immortal nor stationary, and `is_pinned`
/// is precisely the test that separates the two. A refusal writes nothing.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn indy_site_publish(key_addr: i64, val_addr: i64, key: i64, res: i64) {
    if res == 0 {
        return;
    }
    let stable = if (res as u64) >> 40 == 1 {
        true // h|REF_TAG: the per-site singleton handle, a permanent `Vm::indy_singletons` root
    } else {
        crate::nextgen::is_pinned(res as u64)
            && crate::nextgen::is_arena_lambda(unsafe {
                crate::arena::ArenaPtr::from_addr(res as u64)
            })
    };
    if !stable {
        return;
    }
    // SAFETY: both addresses are `&SITE_VALS[i]` / `&SITE_KEYS[i]` baked by `site_entry_addrs`
    // from a `'static` array, so they outlive every compiled method that holds them.
    unsafe {
        let v = &*(val_addr as *const AtomicU64);
        let k = &*(key_addr as *const AtomicU64);
        v.store(res as u64, Relaxed);
        k.store(key as u64, Relaxed);
    }
    SITE_PUBLISHED.fetch_add(1, Relaxed);
}
