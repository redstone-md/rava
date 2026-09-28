//! The structural pure-body SAM fast path — the rung after the trivial-getter arm.
//! Serves a kind-1/3 `(...)I`/`(...)Z` call whose receiver is a kind-6 no-capture
//! heap `Ref` lambda whose RESOLVED implementation — the linkage record, never the
//! caller's pool — is a small verified whitelist body; no synthetic frame.

use crate::*;

crate::diag_counter!(
    pure_sam_served,
    "price",
    "pure_sam_served",
    "structural pure-body SAM fastpath answered a call with no synthetic frame"
);

/// ⭐**KILL SWITCH: `RAVA_NO_PURE_SAM=1`** (or `RAVA_NO_EMIT_MORE=1`), resolved once.
/// Admission is STRUCTURAL, like the ring/getter arms: the implementation triple is
/// the site's own linkage record, the body must match the whitelist exactly, and no
/// guest class or method name is consulted — `DIVERTED_NAMES` is the general path's
/// own seventeen-name JVM key set, not a guest list.
#[inline]
pub(crate) fn pure_sam_enabled() -> bool {
    static ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        std::env::var_os("RAVA_NO_PURE_SAM").is_none() && !crate::flags::emit_more_disabled()
    });
    *ON
}
/// Memo kinds — a property of the MATCHED BODY, never of the descriptor: the
/// `ireturn`ed i32 is packed as the framed tail would (`Z` is an int here; no masking).
mod sk {
    /// Verified int body; the payload is the parameter count `n`.
    pub(super) const BODY_INT: u8 = 0;
    /// Verified structural refusal for exactly this triple (permanent facts only).
    pub(super) const REFUSAL: u8 = 2;
}

/// ⭐**THE TRIPLE-KEYED MEMO, separate from `ring_memo`/`getter_memo` on purpose** —
/// a ring/getter answer must never be read back as a SAM verdict. Direct-mapped,
/// thread-local; stale entries re-verify; the key ends `| 1`, never `u64::MAX`.
mod pure_sam_memo {
    const SLOTS: usize = 128;
    const EMPTY: u64 = u64::MAX;

    struct Table {
        key: [u64; SLOTS],
        kind: [u8; SLOTS],
        payload: [u32; SLOTS],
    }

    thread_local! {
        static T: std::cell::RefCell<Table> = const {
            std::cell::RefCell::new(Table {
                key: [EMPTY; SLOTS],
                kind: [0; SLOTS],
                payload: [0; SLOTS],
            })
        };
    }

    #[inline]
    fn slot(key: u64) -> usize {
        (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 57) as usize & (SLOTS - 1)
    }

    #[inline]
    pub(super) fn get(key: u64) -> Option<(u8, u32)> {
        let i = slot(key);
        T.with(|t| {
            let t = t.borrow();
            (t.key[i] == key).then(|| (t.kind[i], t.payload[i]))
        })
    }

    #[inline]
    pub(super) fn put(key: u64, kind: u8, payload: u32) {
        let i = slot(key);
        T.with(|t| {
            let mut t = t.borrow_mut();
            t.key[i] = key;
            t.kind[i] = kind;
            t.payload[i] = payload;
        });
    }
}

#[inline]
fn pure_sam_memo_key(site_key: u64, target_ptr: u64, midx: u32) -> u64 {
    (site_key ^ target_ptr.rotate_left(17) ^ (midx as u64).rotate_left(43))
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        | 1
}

/// The INVERSE of the getter arm's reading, on purpose: `synchronized` monitor
/// semantics are not reproduced, and a NON-static implementation is refused rather
/// than served through a synthetic receiver. Both flags are immutable class constants.
const ACC_STATIC: u16 = 0x0008;
const ACC_SYNCHRONIZED: u16 = 0x0020;
const ACC_INTERFACE: u16 = 0x0200;
const ACC_ABSTRACT: u16 = 0x0400;

/// ⭐**FRAMEWORK CONSTANTS — the general path's own JVM-level key set, not guest names.**
/// `invokevirtual` diverts a lambda receiver pre-probe on two name-keyed,
/// descriptor-agnostic bits: `gm::OBJECT_METHOD` (Object's own surface — a SAM can
/// never be one) and `gm::FI_DEFAULT` (`invoke_virtual::FI_DEFAULT_NAMES`, which
/// `build_fi_default` answers with a combinator). The fastpath skips the gauntlet,
/// so it declines exactly these — per-crossing, un-memoised. The guard is latent: no
/// classpath FI's unique abstract is one of these with a `([IZ]{0,2})[IZ]` descriptor.
const DIVERTED_NAMES: [&str; 17] = [
    "toString", "hashCode", "getClass", "equals", "wait", "notify", "notifyAll", "andThen",
    "compose", "negate", "and", "or", "reversed", "thenComparing", "thenComparingInt",
    "thenComparingLong", "thenComparingDouble",
];

/// A decoded whitelist instruction; branch targets are forward-only, strictly inside.
#[derive(Clone, Copy)]
enum Insn {
    Iconst { val: i32 },
    Iload { idx: u8 },
    If { taken_iff_nonzero: bool, target: u8 },
    /// Pop value2 then value1; taken iff `value1 CMP value2` (0 eq, 1 ne, 2 lt, 3 ge, 4 gt, 5 le).
    IfIcmp { cmp: u8, target: u8 },
    Goto { target: u8 },
    Ireturn,
}

#[inline]
fn branch_target(bc: &[u8], pc: usize, len: usize) -> Option<u8> {
    if pc + 2 >= len {
        return None;
    }
    let t = pc as i32 + i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32;
    (t > pc as i32 && t < len as i32).then_some(t as u8)
}

/// Decode the whitelist instruction at `pc`. `None` on a non-whitelisted opcode, a
/// truncated operand, or a bad branch target. Returns (instruction, width); `pc < len`.
#[inline]
fn decode(bc: &[u8], pc: usize, len: usize) -> Option<(Insn, u8)> {
    let op = bc[pc];
    match op {
        0x02..=0x08 => Some((Insn::Iconst { val: op as i32 - 0x03 }, 1)),
        0x15 => (pc + 1 < len).then(|| Insn::Iload { idx: bc[pc + 1] }).map(|i| (i, 2)),
        0x1a..=0x1d => Some((Insn::Iload { idx: op - 0x1a }, 1)),
        0x99 | 0x9a => Some((
            Insn::If { taken_iff_nonzero: op == 0x9a, target: branch_target(bc, pc, len)? },
            3,
        )),
        0x9f..=0xa4 => Some((Insn::IfIcmp { cmp: op - 0x9f, target: branch_target(bc, pc, len)? }, 3)),
        0xa7 => Some((Insn::Goto { target: branch_target(bc, pc, len)? }, 3)),
        0xac => Some((Insn::Ireturn, 1)),
        _ => None,
    }
}

/// Shared fall-through edge: the successor pc must land strictly inside the body.
#[inline]
fn fall(p: u8, w: u8, len: usize) -> Option<u8> {
    let p2 = p as usize + w as usize;
    (p2 < len).then_some(p2 as u8)
}

/// Verify the mini-CFG once, at derive time: a linear whitelist decode from pc = 0
/// (ending exactly at `len`), then a worklist DFS over (pc, depth) — targets must be
/// decoded starts (forward-only ⇒ acyclic), `iload` indices < n_params (unset locals
/// refused), depth ≤ min(max_stack, 8), nothing falls off, ONE reachable `ireturn`.
fn verify_cfg(bc: &[u8], max_stack: u16, n_params: usize) -> Option<()> {
    let len = bc.len();
    if len == 0 || len > 16 {
        return None;
    }
    let depth_cap = (max_stack as usize).min(8);
    let mut starts = [false; 16];
    let mut pc = 0usize;
    while pc < len {
        let (_, w) = decode(bc, pc, len)?;
        starts[pc] = true;
        pc += w as usize;
    }
    if pc != len {
        return None;
    }
    let mut visited = [[false; 9]; 16];
    let mut ret_pc: Option<u8> = None;
    let mut stack: Vec<(u8, usize)> = vec![(0, 0)];
    while let Some((p, d)) = stack.pop() {
        if visited[p as usize][d] {
            continue;
        }
        visited[p as usize][d] = true;
        let (insn, w) = decode(bc, p as usize, len)?;
        match insn {
            Insn::Iconst { .. } => {
                let d2 = d + 1;
                if d2 > depth_cap {
                    return None;
                }
                stack.push((fall(p, w, len)?, d2));
            }
            Insn::Iload { idx } => {
                if idx as usize >= n_params {
                    return None;
                }
                let d2 = d + 1;
                if d2 > depth_cap {
                    return None;
                }
                stack.push((fall(p, w, len)?, d2));
            }
            Insn::If { target, .. } => {
                if d < 1 || !starts[target as usize] {
                    return None;
                }
                stack.push((target, d - 1));
                stack.push((fall(p, w, len)?, d - 1));
            }
            Insn::IfIcmp { target, .. } => {
                if d < 2 || !starts[target as usize] {
                    return None;
                }
                stack.push((target, d - 2));
                stack.push((fall(p, w, len)?, d - 2));
            }
            Insn::Goto { target } => {
                if !starts[target as usize] {
                    return None;
                }
                stack.push((target, d));
            }
            Insn::Ireturn => {
                if d == 0 {
                    return None;
                }
                if ret_pc.is_some_and(|prev| prev != p) {
                    return None;
                }
                ret_pc = Some(p);
            }
        }
    }
    ret_pc?;
    Some(())
}

/// Evaluate the verified mini-CFG on concrete i32 args (JVM order: `if_icmp*` pops
/// value2 then value1). Verified, so the 64-step cap and stack checks are pure
/// defense (declines un-memoised).
fn eval_cfg(bc: &[u8], locals: &[i32]) -> Option<i32> {
    let len = bc.len();
    let mut pc = 0usize;
    let mut steps = 0usize;
    let mut stack = [0i32; 9];
    let mut depth = 0usize;
    loop {
        steps += 1;
        if steps > 64 {
            return None;
        }
        let (insn, w) = decode(bc, pc, len)?;
        match insn {
            Insn::Iconst { val } => {
                if depth >= 9 {
                    return None;
                }
                stack[depth] = val;
                depth += 1;
            }
            Insn::Iload { idx } => {
                if depth >= 9 || idx as usize >= locals.len() {
                    return None;
                }
                stack[depth] = locals[idx as usize];
                depth += 1;
            }
            Insn::If { taken_iff_nonzero, target } => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                let v = stack[depth];
                let taken = if taken_iff_nonzero { v != 0 } else { v == 0 };
                if taken {
                    pc = target as usize;
                    continue;
                }
            }
            Insn::IfIcmp { cmp, target } => {
                if depth < 2 {
                    return None;
                }
                depth -= 1;
                let value2 = stack[depth];
                depth -= 1;
                let value1 = stack[depth];
                let taken = match cmp {
                    0 => value1 == value2,
                    1 => value1 != value2,
                    2 => value1 < value2,
                    3 => value1 >= value2,
                    4 => value1 > value2,
                    _ => value1 <= value2,
                };
                if taken {
                    pc = target as usize;
                    continue;
                }
            }
            Insn::Goto { target } => {
                pc = target as usize;
                continue;
            }
            Insn::Ireturn => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                return Some(stack[depth]);
            }
        }
        pc += w as usize;
        if pc >= len {
            return None;
        }
    }
}

/// Serve a BODY_INT row: tag-test every arg cell (one that does not survive
/// `i64 → i32 → i64` is mangled by the general path's truncation too; declining
/// un-memoised keeps both directions bit-exact), then run the mini-CFG.
#[inline]
fn pure_sam_serve(bc: &[u8], raw: &[i64], n: usize) -> Option<i64> {
    for &cell in &raw[1..] {
        if (cell as i32) as i64 != cell {
            return None;
        }
    }
    let mut locals = [0i32; 3];
    for k in 0..n {
        locals[k] = raw[k + 1] as i32;
    }
    let r = eval_cfg(bc, &locals[..n])?;
    pure_sam_served().inc();
    Some(jit::pack_local(&Value::Int(r)))
}

impl Vm {
    /// ⭐**THE STRUCTURAL PURE-BODY SAM ARM.** The triple key (site stable key, target
    /// `Arc` pointer, method index) is all stable process-lifetime identities — the
    /// site `Arc` is pinned on every derive, so it never recycles.
    /// A REFUSAL is memoised only for triple-pure facts (flags, exception table, body
    /// size, whitelist/CFG verdict; the descriptor equality pins the param count). Every
    /// other decline stays UNMEMOISED — not a function of the key: descriptor equality,
    /// FI identity gate, seventeen-name diversion guard (caller-site facts), `$fi`/owner
    /// concrete-default replication (heap/registry state), a resolve `Err`, the null
    /// receiver-word, the arg-cell tag test.
    /// The body check is the whole admission: no exceptions, not synchronized, static,
    /// ≤ 16 bytes, whitelist opcodes only, forward-only branches, depth ≤
    /// `min(max_stack, 8)`, one reachable `ireturn`. Serving runs the verified mini-CFG
    /// over the raw `Int` cells and packs `Value::Int` as the framed tail would;
    /// bypassed side effects are perf-only (no GC rooting — empty captures, int cells,
    /// receiver rooted in the caller's frame). Refusal: `None` on any anomaly — the
    /// crossing continues to the frame unchanged; this arm never throws.
    pub(crate) fn pure_sam_fastpath(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        raw: &[i64],
    ) -> Option<i64> {
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        // The site's ACTUAL selector, from the caller's own pool; then the descriptor
        // gate: `(` + 0..2 of {I, Z} + `)` + one of {I, Z}, exact (`n < 2` makes a
        // 3+-param descriptor fail the `)` check instead of parsing).
        let m = caller.file.pool.member_ref(cp)?;
        let b = m.descriptor.as_bytes();
        if b.first() != Some(&b'(') {
            return None;
        }
        let mut i = 1usize;
        let mut n = 0usize;
        while i < b.len() && n < 2 && (b[i] == b'I' || b[i] == b'Z') {
            n += 1;
            i += 1;
        }
        if b.get(i) != Some(&b')') {
            return None;
        }
        if !matches!(b.get(i + 1), Some(b'I' | b'Z')) {
            return None;
        }
        if i + 2 != b.len() || n + 1 != raw.len() {
            return None;
        }
        // The receiver: same decode as the general path's tail; null/arena kinds
        // decline (the NPE is the general path's).
        let recv = jit::unbox_ref(self, *raw.first()?, m.owner);
        let Value::Ref(h) = recv else {
            return None;
        };
        if !self.heap.is_lambda(h) {
            return None;
        }
        let (lkind, site, captured) = self.heap.lambda_header(h)?;
        if lkind != 6 || !captured.is_empty() {
            return None;
        }
        // Per-crossing caller-site fact: a different call site may reach this interned
        // site with a different descriptor (`sam_box_return` stays an identity).
        if &*site.descriptor != m.descriptor {
            return None;
        }
        // Caller-site FI identity: the receiver's own FI must declare exactly one
        // abstract method, BE the caller's selector — a true SAM invocation. The
        // general path diverts the seventeen (see `DIVERTED_NAMES`) pre-probe, so a
        // crossing under one is never a SAM serve; checked before the memo, guarding
        // both serve paths (hit and derive).
        let Value::Str(fi) = self.heap.get_field(h, "$fi") else {
            return None;
        };
        if DIVERTED_NAMES.contains(&m.name) || !self.fi_declares_sam(&fi, m.name, m.descriptor) {
            return None;
        }
        // The gauntlet's concrete-default replication, verbatim: resolvable real code
        // declines (the general path invokes THAT); a resolve `Err` declines too.
        for cls in [m.owner, fi.as_str()] {
            match self.resolve_method(cls, m.name, m.descriptor) {
                Ok(Some(decl)) => {
                    if decl.file.method(m.name, m.descriptor).is_some_and(|x| x.code.is_some()) {
                        return None;
                    }
                }
                Ok(None) => {}
                Err(_) => return None,
            }
        }
        // The implementation triple from the linkage record, resolved exactly as the
        // general path's kind-6 arm does; an `Err` declines (re-raised there).
        let Ok(Some(target)) = self.resolve_method(&site.owner, &site.name, &site.descriptor) else {
            return None;
        };
        let midx = crate::tlcache::method_index(&target, &site.name, &site.descriptor)?;
        let site_key = self.heap.lambda_site_key(h);
        if site_key == 0 {
            return None;
        }
        // No force-native decline, by STRUCTURAL argument. (a) The whitelist admits
        // only a pure int function — no field/method/array/allocation opcode exists
        // in it — so a body needing the native machinery behind any fold (Unsafe,
        // view iterators, select loops) fails decode by construction. (b) The
        // diverted ∩ served population is empty: a whole-classpath scan (40 jars,
        // 30 287 classes, 23 339 lambda sites) found two static `([IZ]{0,2})[IZ]`
        // methods on force-native owners (49/33-byte bodies — over the 16-byte cap)
        // and two kind-6 sites hitting the fold (both descriptor-gate failures).
        // (c) A future jar breaking (b) is still folded per-site, permanently, by the
        // general path's plan (`decl: None`), whose natives arm needs a `Ref` in
        // args[0] that a kind-6 static no-capture apply never feeds it.
        let mkey = pure_sam_memo_key(site_key, Arc::as_ptr(&target) as u64, midx);
        match pure_sam_memo::get(mkey) {
            // Verified refusal — permanent; below: a verified int body (payload = n).
            Some((sk::REFUSAL, _)) => return None,
            Some((sk::BODY_INT, payload)) if payload == n as u32 => {
                let code = target.file.methods.get(midx as usize).and_then(|mi| mi.code.as_ref())?;
                return pure_sam_serve(&code.bytecode, raw, n);
            }
            Some(_) => return None,
            None => {}
        }
        // Pin the site so the memo key never dangles (insert-once, permanent).
        crate::nextgen::lambda_site_pin(site);
        let mi = target.file.methods.get(midx as usize)?;
        let Some(code) = mi.code.as_ref() else {
            pure_sam_memo::put(mkey, sk::REFUSAL, 0);
            return None;
        };
        if mi.access_flags & ACC_SYNCHRONIZED != 0
            || mi.access_flags & ACC_STATIC == 0
            || !code.exceptions.is_empty()
            || code.bytecode.len() > 16
            || verify_cfg(&code.bytecode, code.max_stack, n).is_none()
        {
            pure_sam_memo::put(mkey, sk::REFUSAL, 0);
            return None;
        }
        pure_sam_memo::put(mkey, sk::BODY_INT, n as u32);
        pure_sam_serve(&code.bytecode, raw, n)
    }

    /// Structural SAM identity: `fi` is a loaded interface whose unique abstract
    /// method is exactly the caller's (name, descriptor).
    fn fi_declares_sam(&self, fi: &str, name: &str, desc: &str) -> bool {
        let Ok(cls) = self.loader.get_or_load(fi) else {
            return false;
        };
        let mut abs = cls.file.methods.iter().filter(|m| m.access_flags & ACC_ABSTRACT != 0);
        match (abs.next(), abs.next()) {
            (Some(mi), None) => {
                cls.file.access_flags & ACC_INTERFACE != 0 && mi.name == name && mi.descriptor == desc
            }
            _ => false,
        }
    }
}
