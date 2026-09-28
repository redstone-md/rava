//! Caller attribution for the RIP profiler (`RAVA_PROFILE_PC_STACKS`, default OFF).
//!
//! WHY THIS EXISTS. `pc_profile` samples the guest thread's instruction pointer, which names
//! the LEAF — the function the CPU is in. That is enough to price a cost and useless for
//! removing one: the boot's largest unowned host row was `str::contains` (`memchr` /
//! `is_contained_in` / `next_match`) at ~9% of Block B, and a leaf name says only that
//! somebody, somewhere, is doing substring search. Greps over the tree find ~240 `.contains(`
//! sites; the profile cannot tell which of them the boot actually executes.
//!
//! HOW IT ATTRIBUTES, AND WHY NOT `StackWalk64`. A real unwind (`RtlLookupFunctionEntry` +
//! `RtlVirtualUnwind`) has to run INSIDE the suspend window, because the stack below RSP is
//! overwritten as soon as the thread resumes — and `RtlLookupFunctionEntry` can take the
//! loader's function-table lock, which the frozen thread may hold across a module load or a
//! `RtlAddFunctionTable` from the JIT. That is a real deadlock, and this profiler's whole
//! design rule (see `pc_profile`) is that nothing may block while the guest is suspended.
//!
//! So the suspend window does one extra SYSCALL — copy `STACK_BYTES` from RSP — and every
//! decision is made after the thread is running again. The copy is then scanned
//! conservatively: a word is a candidate return address when it points into committed,
//! executable memory AND the bytes immediately before it decode as a `call` (`E8 rel32`, or
//! `FF /2` / `FF /3` at any of the six lengths an indirect call takes). Both tests are pure
//! reads of immutable code, so they are safe at leisure.
//!
//! WHAT THE RESULT IS AND IS NOT. A conservative scan recovers a SUPERSET of the true frames
//! — stale return addresses left by earlier, deeper calls survive in dead stack slots. It is
//! therefore not a call tree and must not be read as one. It is a per-frame occurrence
//! histogram over many samples, and that is enough to name a caller, because a stale address
//! appears in the slice of samples that happen to follow one particular earlier call while
//! the true caller appears in ALL of them. Rank by share and read the top row.
//!
//! Frames that land in compiled code are named by the guest method that owns the range, so a
//! host cost can be attributed to the Java method that provoked it as well as to the Rust
//! function that pays it.
//!
//! Windows-only, like the sampler it extends; elsewhere every entry point is a no-op.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Bytes of guest stack copied per sample. Deep enough to span the handful of small Rust
/// frames between a leaf like `memchr` and the engine routine that called it, small enough
/// that the copy is a rounding error against a 10 ms tick.
pub(crate) const STACK_BYTES: usize = 8192;

/// Candidate return addresses kept per sample, nearest (innermost) first.
const MAX_FRAMES: usize = 32;

/// One sample: when it was taken, the leaf the CPU was executing, and the candidate return
/// addresses recovered from its stack, ordered from the innermost frame outwards.
struct Stack {
    t_ms: u64,
    rip: usize,
    frames: Vec<usize>,
}

static STACKS: Mutex<Vec<Stack>> = Mutex::new(Vec::new());

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_PROFILE_PC_STACKS").is_some())
}

/// Comma-separated substrings; a sample is reported only when its LEAF symbol matches one of
/// them. Empty = report every sample, which is the right setting for a first look and the
/// wrong one once a target is known (the histogram then mixes every host cost together).
fn leaf_filter() -> &'static [String] {
    static F: OnceLock<Vec<String>> = OnceLock::new();
    F.get_or_init(|| {
        std::env::var("RAVA_PC_STACK_FILTER")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// Scan one captured stack window and keep the candidate return addresses. Called from the
/// sampler thread AFTER the guest has been resumed — nothing here may run while it is frozen.
pub fn record(t_ms: u64, rip: usize, stack: &[u8]) {
    if !enabled() {
        return;
    }
    let mut frames = Vec::with_capacity(8);
    for w in stack.chunks_exact(8) {
        if frames.len() >= MAX_FRAMES {
            break;
        }
        let a = usize::from_le_bytes([w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7]]);
        // Cheap rejects first: a user-mode code address is never null and never in the
        // kernel half, and `VirtualQuery` is far too expensive to run on every stack word.
        if a < 0x1_0000 || a >= 0x8000_0000_0000 {
            continue;
        }
        if !exec_page(a) {
            continue;
        }
        if !preceded_by_call(a) {
            continue;
        }
        frames.push(a);
    }
    if let Ok(mut v) = STACKS.lock() {
        v.push(Stack { t_ms, rip, frames });
    }
}

/// Page-granular memoisation of `VirtualQuery`. Sampler-thread-local: this module has exactly
/// one writer by construction, so the map needs no lock and never contends with the guest.
fn exec_page(addr: usize) -> bool {
    std::thread_local! {
        static PAGES: std::cell::RefCell<HashMap<usize, bool>> =
            std::cell::RefCell::new(HashMap::new());
    }
    PAGES.with(|p| {
        *p.borrow_mut()
            .entry(addr >> 12)
            .or_insert_with(|| crate::pc_profile::is_executable(addr))
    })
}

/// True when the bytes ending at `addr` decode as a `call`, i.e. `addr` is plausibly the
/// address a `ret` would return to. Rejects the majority of stale/coincidental code pointers
/// that a conservative scan would otherwise report.
///
/// Only reads inside `addr`'s own page, which `exec_page` has already established is
/// committed — so this can never fault, and it never needs the page's neighbour to be mapped.
fn preceded_by_call(addr: usize) -> bool {
    if addr & 0xFFF < 8 {
        return false;
    }
    // SAFETY: `addr-8 .. addr` lies inside a committed, executable page (checked by the
    // caller), and code pages are immutable for the process lifetime.
    let b = unsafe { std::slice::from_raw_parts((addr - 8) as *const u8, 8) };
    // `E8 rel32` — the direct call, 5 bytes, so its opcode sits at addr-5.
    if b[3] == 0xE8 {
        return true;
    }
    // `FF /2` (call r/m64) and `FF /3` (call far m) take 2..7 bytes depending on ModRM, SIB,
    // displacement and REX, so the opcode can be at any of addr-2 .. addr-7.
    for k in 2..=7usize {
        let i = 8 - k;
        if b[i] == 0xFF && (b[i + 1] >> 3) & 7 >= 2 && (b[i + 1] >> 3) & 7 <= 3 {
            return true;
        }
    }
    false
}

/// Target of the direct `call` that `addr` returns from, when there is one. Only `E8 rel32`
/// is decodable statically; an indirect `call r/m64` has no target in the instruction stream
/// and yields `None`, which is what bounds the verified walk below.
fn direct_call_target(addr: usize) -> Option<usize> {
    if addr & 0xFFF < 8 {
        return None;
    }
    // SAFETY: as `preceded_by_call` — inside a committed executable page, immutable code.
    let b = unsafe { std::slice::from_raw_parts((addr - 8) as *const u8, 8) };
    if b[3] != 0xE8 {
        return None;
    }
    let rel = i32::from_le_bytes([b[4], b[5], b[6], b[7]]) as i64;
    // rel32 is relative to the NEXT instruction, which is exactly the return address.
    Some((addr as i64).wrapping_add(rel) as usize)
}

/// Entry address of the function containing `pc`: the PDB's for a host address, the range
/// start for compiled code. Memoised — dbghelp lookups are the dominant cost of a dump.
fn base_of(pc: usize, ranges: &[(usize, usize, u32)], cache: &mut HashMap<usize, usize>) -> usize {
    if let Some(&b) = cache.get(&pc) {
        return b;
    }
    let i = ranges.partition_point(|r| r.0 <= pc);
    let b = match i.checked_sub(1).map(|j| ranges[j]) {
        Some((start, end, _)) if pc < end => start,
        _ => crate::pc_profile::symbol_base(pc),
    };
    cache.insert(pc, b);
    b
}

/// Recover the caller chain of `rip` from the scanned candidates, keeping only links that can
/// be PROVEN: a frame is accepted when the direct `call` it returns from targets the entry of
/// the function the frame below it is executing. That is a real unwind constraint, not a
/// heuristic — a stale stack word passes only if it happens to be a return address from a
/// direct call to the very same function, which the aggregate then shows as a minority row.
///
/// The walk stops at the first frame it cannot prove (an indirect call, or a caller whose own
/// call site was indirect), so a short chain is honest rather than padded. `MAX_DEPTH` bounds
/// the cost; naming the immediate caller only needs the first link.
fn verified_chain(
    rip: usize,
    frames: &[usize],
    ranges: &[(usize, usize, u32)],
    bases: &mut HashMap<usize, usize>,
    names: &mut HashMap<usize, String>,
) -> Vec<String> {
    const MAX_DEPTH: usize = 6;
    let mut out = Vec::new();
    let mut expect = base_of(rip, ranges, bases);
    let mut from = 0usize;
    while out.len() < MAX_DEPTH && expect != 0 {
        let Some(k) = frames[from..].iter().position(|&f| {
            direct_call_target(f).is_some_and(|t| base_of(t, ranges, bases) == expect)
        }) else {
            break;
        };
        let f = frames[from + k];
        out.push(name_of(f, ranges, names));
        expect = base_of(f.saturating_sub(1), ranges, bases);
        from += k + 1;
    }
    out
}

/// Names a recovered address: the guest method when it lands in compiled code, the Rust/system
/// symbol otherwise. Return addresses point AFTER the call, so the lookup uses `pc-1` to stay
/// inside the calling instruction — otherwise a call in a function's last instruction is
/// attributed to whatever follows it.
fn name_of(pc: usize, ranges: &[(usize, usize, u32)], cache: &mut HashMap<usize, String>) -> String {
    if let Some(n) = cache.get(&pc) {
        return n.clone();
    }
    let probe = pc.saturating_sub(1);
    let i = ranges.partition_point(|r| r.0 <= probe);
    let n = match i.checked_sub(1).map(|j| ranges[j]) {
        Some((_, end, mid)) if probe < end => format!(
            "<jit> {}",
            crate::jit::compiled_method_name(mid).unwrap_or_else(|| format!("mid {mid}"))
        ),
        _ => {
            let s = crate::pc_profile::symbolize(probe);
            s.rsplit_once('+').map_or(s.clone(), |(f, _)| f.to_string())
        }
    };
    cache.insert(pc, n.clone());
    n
}

/// Rank a `(name -> count)` tally and print the top rows as a share of `den`.
/// How many rows a tally prints (`RAVA_PC_TOP`, default 40).
///
/// ⛔**The default was 10, and 10 rows of this profile cover 40.8% of the samples** — so the
/// other 59% was invisible and the table read as if the top row were the whole story. That is
/// the same truncation the ledger already recorded for four other samplers, arrived at again.
/// The printer now also states the coverage it achieved, so a truncated table cannot be mistaken
/// for a complete one.
fn tally_top(default: usize) -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let n = *N.get_or_init(|| {
        std::env::var("RAVA_PC_TOP").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0)
    });
    if n == 0 { default } else { n }
}

fn print_tally(label: &str, tally: HashMap<String, u64>, den: u64, top: usize) {
    let top = tally_top(top.max(40));
    let mut rows: Vec<(String, u64)> = tally.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let shown: u64 = rows.iter().take(top).map(|(_, n)| *n).sum();
    eprintln!(
        "[PC-STACK]   -- {label} — {} rows shown, {:.1}% of {} samples covered --",
        rows.len().min(top),
        shown as f64 * 100.0 / den.max(1) as f64,
        den
    );
    for (name, n) in rows.iter().take(top) {
        eprintln!(
            "[PC-STACK]   {:>5.1}%  {:>7}  {}",
            *n as f64 * 100.0 / den.max(1) as f64,
            n,
            name
        );
    }
}

/// Deduped stack frames kept on a trace line. Enough to carry the handful of engine frames
/// between a leaf and the guest method that provoked it, bounded so the trace of a 200 s boot
/// stays in the tens of megabytes.
const TRACE_FRAMES: usize = 12;

/// One line per sample:
///
/// ```text
/// [PC-TRACE] <epoch_ms> <leaf> | <verified chain, innermost first> | <frames on the stack>
/// ```
///
/// Field 2 is PROVEN (each link's direct `call` targets the entry of the frame below it) and
/// often empty — a leaf reached through an IAT thunk can never be proven. Field 3 is the
/// conservative scan: a superset that includes stale slots, correct only in aggregate. Both
/// are carried because they answer different questions, and which one a given cost needs is
/// not knowable until the cost is known.
fn trace_lines(stacks: &[Stack], ranges: &[(usize, usize, u32)], cache: &mut HashMap<usize, String>) {
    let mut bases: HashMap<usize, usize> = HashMap::new();
    for s in stacks {
        let leaf = name_of(s.rip, ranges, cache);
        let chain = verified_chain(s.rip, &s.frames, ranges, &mut bases, cache).join(" < ");
        let mut seen: Vec<String> = Vec::with_capacity(TRACE_FRAMES);
        for &f in &s.frames {
            if seen.len() >= TRACE_FRAMES {
                break;
            }
            let n = name_of(f, ranges, cache);
            if !seen.contains(&n) {
                seen.push(n);
            }
        }
        eprintln!("[PC-TRACE] {} {} | {} | {}", s.t_ms, leaf, chain, seen.join(";"));
    }
}

/// Print the caller attribution for this dump block and drain the buffer, so each block covers
/// exactly the interval since the previous dump (same contract as `pc_profile::dump`).
pub fn dump(header: &str, ranges: &[(usize, usize, u32)]) {
    if !enabled() {
        return;
    }
    let stacks: Vec<Stack> = match STACKS.lock() {
        Ok(mut v) => std::mem::take(&mut v),
        Err(_) => return,
    };
    if stacks.is_empty() {
        return;
    }
    let filter = leaf_filter();
    let mut cache: HashMap<usize, String> = HashMap::new();
    if crate::pc_profile::trace() {
        trace_lines(&stacks, ranges, &mut cache);
        return;
    }

    let matched: Vec<&Stack> = stacks
        .iter()
        .filter(|s| {
            if filter.is_empty() {
                return true;
            }
            let leaf = name_of(s.rip, ranges, &mut cache).to_ascii_lowercase();
            filter.iter().any(|f| leaf.contains(f.as_str()))
        })
        .collect();
    eprintln!(
        "[PC-STACK {header} BLOCK] {} stacks captured, {} match leaf filter {:?} ({:.1}%)",
        stacks.len(),
        matched.len(),
        filter,
        matched.len() as f64 * 100.0 / stacks.len() as f64
    );
    if matched.is_empty() {
        return;
    }
    let den = matched.len() as u64;

    // Which leaves the filter actually caught — a filter is a claim about symbol names and
    // this is the line that shows what it selected.
    let mut leaves: HashMap<String, u64> = HashMap::new();
    // Cross-tabbed by LEAF: one filter usually selects several distinct costs (a char
    // search and a substring search are different defects with different fixes), and a
    // pooled caller table mixes them into an unactionable average.
    let mut caller: HashMap<String, HashMap<String, u64>> = HashMap::new();
    let mut chains: HashMap<String, u64> = HashMap::new();
    let mut anywhere: HashMap<String, u64> = HashMap::new();
    let mut bases: HashMap<usize, usize> = HashMap::new();
    let mut unproven: HashMap<String, u64> = HashMap::new();
    // A leaf that is a DLL export (`memchr`, `RtlQueryEnvironmentVariable`) is entered
    // through an IAT thunk — `call [__imp_x]` — and an indirect call carries no target to
    // check against, so NO frame can ever be proven for it. Falling silent there would hide
    // the single largest row. For those samples the innermost recovered frame is reported
    // instead, clearly labelled: it is the first call-preceded word above RSP, which is the
    // true return address unless a stale slot sits below it.
    let mut plausible: HashMap<String, HashMap<String, u64>> = HashMap::new();
    for s in &matched {
        let leaf = name_of(s.rip, ranges, &mut cache);
        *leaves.entry(leaf.clone()).or_insert(0) += 1;
        let chain = verified_chain(s.rip, &s.frames, ranges, &mut bases, &mut cache);
        match chain.first() {
            Some(c) => {
                *caller.entry(leaf).or_default().entry(c.clone()).or_insert(0) += 1;
            }
            None => {
                *unproven.entry(leaf.clone()).or_insert(0) += 1;
                if let Some(&f) = s.frames.first() {
                    let n = name_of(f, ranges, &mut cache);
                    *plausible.entry(leaf).or_default().entry(n).or_insert(0) += 1;
                }
            }
        }
        if !chain.is_empty() {
            *chains.entry(chain.join(" < ")).or_insert(0) += 1;
        }
        // Deduped per sample: a recursive or repeatedly-inlined frame must not out-vote a
        // caller that appears once, since the question is "in what fraction of the samples
        // is this function on the stack".
        let mut seen: Vec<String> = Vec::with_capacity(s.frames.len());
        for &f in &s.frames {
            let n = name_of(f, ranges, &mut cache);
            if !seen.contains(&n) {
                seen.push(n);
            }
        }
        for n in &seen {
            *anywhere.entry(n.clone()).or_insert(0) += 1;
        }
    }
    let mut by_leaf: Vec<(String, u64)> = leaves.iter().map(|(k, &v)| (k.clone(), v)).collect();
    by_leaf.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    print_tally("LEAF (what the filter selected)", leaves, den, 10);
    for (leaf, n) in by_leaf.iter().take(6) {
        let u = unproven.get(leaf).copied().unwrap_or(0);
        print_tally(
            &format!(
                "VERIFIED callers of `{leaf}` ({n} samples, {u} unprovable — reached by an \
                 indirect call)"
            ),
            caller.remove(leaf).unwrap_or_default(),
            *n,
            12,
        );
        if u > 0 {
            print_tally(
                &format!("  ...and the innermost frame of those {u} UNPROVABLE `{leaf}` samples"),
                plausible.remove(leaf).unwrap_or_default(),
                u,
                12,
            );
        }
    }
    print_tally("VERIFIED chain, innermost first", chains, den, 15);
    print_tally("ON THE STACK ANYWHERE — conservative, includes stale slots", anywhere, den, 20);
}
