//! **WHICH OPERAND AT AN INVOKE SITE IS A CLOSURE THIS BODY MINTED — a proof, not a profile.**
//!
//! ⛔⭐**VERDICT FIRST, BECAUSE IT IS THE RESULT: THE MECHANISM WORKS AND THE POPULATION IS ZERO.**
//! Built, measured on a warm `ReloadBench`, and left OPT-IN (`RAVA_CTXINLINE=1`, see `enabled`).
//! The scan proves **4,375** locally minted closures a run and finds **1,503** handed straight
//! into a call in the same basic block; **0** of them are ever APPLIED by the body that minted
//! them, and **1,432** of the rest go into a call the inliner does not splice — so there is no
//! spliced body whose interior site could be re-decided, and nothing is admitted. The numbers and
//! what blocks them are in `enabled`'s doc; the machinery below is kept because the blocker it
//! names sits in a DIFFERENT file, and whoever moves that gate will want this pass already built.
//!
//! ## What was being attacked
//!
//! ⭐The inline census says the dominant refusal in the whole inliner is *"no receiver profile"*:
//! on a warm `ReloadBench` **17,335 of 17,439 `invokeinterface` SAM offers** are refused by
//! `sam.no-profile`, and the learn hook built to fill that table measured ZERO — the sites are not
//! unprofiled, they are SATURATED TO REFUSED, because the profile key is `(caller class,
//! constant-pool index)` and `DataResult$Success @cp27` is ONE `Function.apply` Methodref shared
//! by `map`, `mapOrElse`, `flatMap` and `ap` while `map` is called with every codec lambda in
//! Minecraft.
//!
//! ⭐⭐**AND A PROFILE WAS NEVER NEEDED FOR THAT QUESTION.** Everything the SAM splice wants — the
//! MethodHandle kind, the implementation's owner/name/descriptor, the capture count and the
//! capture kinds — is an OPERAND OF THE `invokedynamic` INSTRUCTION (JVMS 4.7.23: the
//! `BootstrapMethods` entry names `LambdaMetafactory` and its static argument 1 IS the
//! implementation method handle). `opcodes::indy` reads exactly those two fields to build the
//! `Plan::Lambda` the interpreter mints from; this module reads the same two fields at COMPILE
//! time. Both derived identities are pure functions of them:
//!
//!   * `site_idx` — `LambdaSite` is CONTENT-interned over (owner, name, descriptor)
//!     (`nextgen::lambdas::lambda_site_intern`), so interning the statically decoded triple
//!     yields the very index the mint will write into `AL_SITE_SLOT`;
//!   * `cid` — `lambda_class_id(cap_kinds)` is a pure function of the capture-kind vector, which
//!     is a pure function of the indy descriptor (`arena_lambda_shape` derives it the same way).
//!
//! What this module adds is the missing half: WHICH operand of a call is that closure. That is a
//! dataflow fact about the caller's own bytecode — "this value was pushed by the `invokedynamic`
//! at pc P of this very body, and nothing has stored it away since" — and it is exactly the
//! evidence the project's NO-ADAPT rule allows: structural, derived from the instruction stream,
//! never from a class name, a workload, a hash or a timing.
//!
//! ⚠**PROVENANCE IS A SELECTOR, NOT THE SAFETY ARGUMENT.** The emitted shape is still the guarded
//! one `emit_sam_site_guard` already builds — arena-ness, then `class_id == cid`, then
//! `AL_SITE_SLOT == site_idx`, then an untagged test per reference capture — with the miss arm
//! being the site's ordinary interface dispatch. So a provenance answer that is merely WRONG
//! costs the compares and runs the original call; it can never run another site's body. That is
//! why the scan below may be conservative wherever it likes and still be correct.
//!
//! ⛔**THE SCAN STOPS DEAD THE MOMENT IT LOSES STACK SYNC.** Every fact it reports was derived
//! while its abstract operand stack provably matched the real one: an unmodelled opcode, a stack
//! underflow, or a basic-block leader reached with a non-empty stack ends the walk, and only the
//! facts already recorded survive (`scan.stopped-before-end` counts them — about half of 25,000
//! bodies a run). A value that arrives through a block parameter — the "phi of two mints" the
//! design brief names — therefore has NO provenance, because local provenance is cleared at every
//! leader and no stack token crosses one.

use crate::*;
use rava_classfile::{Code, ConstantPool};

/// The statically decoded identity of one `invokedynamic` lambda site, in the form the SAM splice
/// needs. Every field is a constant of the instruction; nothing here is observed at runtime.
#[cfg(feature = "next-gen-gc")]
pub(crate) struct LamFacts {
    /// `MethodHandle` reference kind of the implementation (JVMS 4.4.8).
    pub(crate) kind: u8,
    pub(crate) impl_owner: String,
    pub(crate) impl_name: String,
    pub(crate) impl_desc: String,
    /// Capture-kind nibbles, derived from the indy descriptor exactly as `arena_lambda_shape`
    /// derives them, so the class id below is the one the mint registers.
    pub(crate) cap_kinds: Vec<u8>,
    /// The arena class id for that capture signature — what the guard compares.
    pub(crate) cid: u32,
    /// The content-interned `LambdaSite` index — what `AL_SITE_SLOT` will hold.
    pub(crate) site_idx: u32,
}

/// One invoke site's incoming operands, named by the `invokedynamic` cp index that produced them
/// (`None` = not a locally minted closure, or not proven).
///
/// Position 0 is the RECEIVER for `0xb6`/`0xb7`/`0xb9`; `0xb8` has no receiver and starts at
/// argument 0. This mirrors the emitter's operand-stack window exactly (ONE entry per value, a
/// wide argument included), because the consumer of these facts is the emitter's own binding.
#[derive(Clone)]
pub(crate) struct SiteOperands {
    pub(crate) cp: u16,
    pub(crate) op: u8,
    /// `positions[0]` = receiver when the opcode has one.
    pub(crate) positions: Vec<Option<u16>>,
}

/// Walk `code` with an EXACT abstract operand stack of provenance tokens and record, for every
/// invoke site, which of its incoming operands came from an `invokedynamic` in this same body.
///
/// Returns a map keyed by the invoke's PC. The walk is best-effort and fail-stop: it returns the
/// facts gathered before the first thing it cannot model, never a guess past it.
pub(crate) fn scan_sites(
    code: &Code,
    pool: &ConstantPool,
) -> std::collections::HashMap<usize, SiteOperands> {
    use crate::vm::inline_census as cen;
    let bc = &code.bytecode;
    let mut out = std::collections::HashMap::new();
    cen::note(cen::D_SCAN_BODY);
    let Some(leaders) = jit::find_leaders(bc) else {
        cen::note(cen::D_SCAN_NOLEAD);
        cen::note(cen::D_SCAN_STOP);
        return out;
    };
    let leaders: std::collections::HashSet<usize> = leaders.into_iter().collect();
    // Operand stack of provenance tokens, ONE entry per value (long/double included) — the
    // emitter's convention, so a recorded position maps straight onto its `SV`.
    let mut st: Vec<Option<u16>> = Vec::new();
    // Local-slot provenance, CLEARED AT EVERY LEADER. A slot written on one path and read on
    // another is exactly the merge this analysis refuses to reason about.
    let mut locals: Vec<Option<u16>> = vec![None; code.max_locals as usize + 1];
    let mut pc = 0usize;
    macro_rules! stop {
        () => {{
            cen::note(cen::D_SCAN_STOP);
            return out;
        }};
    }
    macro_rules! pop {
        () => {
            match st.pop() {
                Some(v) => v,
                None => stop!(),
            }
        };
    }
    macro_rules! popn {
        ($n:expr) => {{
            let n: usize = $n;
            if st.len() < n {
                stop!();
            }
            st.truncate(st.len() - n);
        }};
    }
    while pc < bc.len() {
        if pc != 0 && leaders.contains(&pc) {
            if !st.is_empty() {
                stop!(); // a merge with live operands — the walk cannot model it
            }
            locals.iter_mut().for_each(|s| *s = None);
        }
        let op = bc[pc];
        let Some(len) = jit::insn_len_at(bc, pc) else { stop!() };
        match op {
            0x00 => {}
            0x01..=0x11 => st.push(None), // aconst_null .. sipush
            0x12..=0x14 => st.push(None), // ldc / ldc_w / ldc2_w
            0x15..=0x18 => st.push(None), // iload/lload/fload/dload
            0x19 => st.push(locals.get(bc[pc + 1] as usize).copied().flatten()),
            0x1a..=0x29 => st.push(None), // i/l/f/d load_0..3
            0x2a..=0x2d => st.push(locals.get((op - 0x2a) as usize).copied().flatten()),
            0x2e..=0x35 => {
                popn!(2);
                st.push(None);
            }
            0x36..=0x39 => {
                pop!();
            }
            0x3a => {
                let t = pop!();
                if t.is_some() {
                    cen::note(cen::D_TOK_STORE);
                }
                let s = bc[pc + 1] as usize;
                if s < locals.len() {
                    locals[s] = t;
                }
            }
            0x3b..=0x4a => {
                pop!();
            }
            0x4b..=0x4e => {
                let t = pop!();
                if t.is_some() {
                    cen::note(cen::D_TOK_STORE);
                }
                let s = (op - 0x4b) as usize;
                if s < locals.len() {
                    locals[s] = t;
                }
            }
            0x4f..=0x56 => popn!(3), // array stores
            0x57 => {
                pop!();
            }
            0x58 => popn!(2),
            0x59 => {
                let t = *match st.last() {
                    Some(t) => t,
                    None => stop!(),
                };
                st.push(t);
            }
            0x5a => {
                // dup_x1
                if st.len() < 2 {
                    stop!();
                }
                let t = st[st.len() - 1];
                st.insert(st.len() - 2, t);
            }
            0x5c => {
                // dup2 — modelled only for the TWO-VALUE form; a wide value is one token here,
                // so the one-value form would desynchronise. Category is not known without the
                // verifier's type state, so stop rather than guess.
                stop!();
            }
            0x5f => {
                if st.len() < 2 {
                    stop!();
                }
                let n = st.len();
                st.swap(n - 1, n - 2);
            }
            0x60..=0x73 | 0x78..=0x83 | 0x94..=0x98 => {
                popn!(2);
                st.push(None);
            }
            0x74..=0x77 | 0x85..=0x93 => {
                pop!();
                st.push(None);
            }
            0x84 => {}
            0x99..=0x9e | 0xc6 | 0xc7 => {
                pop!();
            }
            0x9f..=0xa6 => popn!(2),
            0xa7 | 0xc8 => {}
            0xaa | 0xab => {
                pop!();
            }
            0xac..=0xb0 => {
                pop!();
            }
            0xb1 => {}
            0xb2 => st.push(None),
            0xb3 => {
                pop!();
            }
            0xb4 => {
                pop!();
                st.push(None);
            }
            0xb5 => popn!(2),
            0xb6..=0xb9 => {
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                let Some(m) = pool.member_ref(idx) else { stop!() };
                let desc = m.descriptor.to_string();
                let nargs = crate::count_args(&desc);
                let recv = usize::from(op != 0xb8);
                if st.len() < nargs + recv {
                    stop!();
                }
                cen::note(cen::D_SCAN_SITE);
                let base = st.len() - nargs - recv;
                if recv == 1 && st[base].is_some() {
                    cen::note(cen::D_TOK_RECV);
                }
                if st[base + recv..].iter().any(|t| t.is_some()) {
                    cen::note(cen::D_TOK_ARG);
                }
                out.insert(
                    pc,
                    SiteOperands { cp: idx, op, positions: st[base..].to_vec() },
                );
                st.truncate(base);
                if jit::ret_jt(&desc).is_some() {
                    st.push(None);
                }
            }
            0xba => {
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                let Some((_, _, desc)) = pool.invoke_dynamic(idx) else { stop!() };
                let desc = desc.to_string();
                let nargs = crate::count_args(&desc);
                if st.len() < nargs {
                    stop!();
                }
                st.truncate(st.len() - nargs);
                if jit::ret_jt(&desc).is_some() {
                    // The TOKEN. Whether the site is a `LambdaMetafactory` one at all is decided
                    // by `lambda_facts` when the token is consumed — this walk only has to say
                    // WHICH instruction produced the value.
                    cen::note(cen::D_SCAN_INDY);
                    st.push(Some(idx));
                }
            }
            0xbb => st.push(None),
            0xbc | 0xbd => {
                pop!();
                st.push(None);
            }
            0xbe => {
                pop!();
                st.push(None);
            }
            0xc0 => {
                // checkcast returns its operand unchanged — the token survives, which is what
                // makes `flatMap`'s `checkcast DataResult` transparent.
            }
            0xc1 => {
                pop!();
                st.push(None);
            }
            0xc2 | 0xc3 => {
                pop!();
            }
            0xc5 => {
                let dims = bc[pc + 3] as usize;
                popn!(dims);
                st.push(None);
            }
            _ => stop!(), // athrow, jsr/ret, wide, dup2_x*, dup_x2, anything unmodelled
        }
        pc += len;
    }
    out
}

/// Decode the `invokedynamic` at `cp` of `class` into the facts a SAM splice needs, or `None`
/// when the site is not a `LambdaMetafactory` lambda mint.
///
/// ⚠**THIS IS THE SAME TWO POOL READS `opcodes::indy` MAKES**, in the same order, against the
/// same constant: the bootstrap's own method handle decides the site's MEANING (JVMS 4.7.23), and
/// static argument 1 IS the implementation method handle for both `metafactory` and
/// `altMetafactory`. Nothing here inspects a guest identifier.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn lambda_facts(
    vm: &mut Vm,
    class: &std::sync::Arc<LoadedClass>,
    cp: u16,
) -> Option<LamFacts> {
    let (bsm, _, desc) = class.file.pool.invoke_dynamic(cp)?;
    let desc = desc.to_string();
    let bm = class.file.bootstrap_methods.get(bsm as usize)?;
    if class.file.pool.method_handle(bm.handle_index)?.owner
        != "java/lang/invoke/LambdaMetafactory"
    {
        return None;
    }
    let mh = *bm.args.get(1)?;
    let (kind, m) = class.file.pool.method_handle_full(mh)?;
    let (impl_owner, impl_name, impl_desc) =
        (m.owner.to_string(), m.name.to_string(), m.descriptor.to_string());
    // The capture kinds, derived from the indy descriptor by the SAME function
    // `arena_lambda_shape` uses — one parse, so the class id cannot drift from the mint's.
    let types = crate::opcodes::concat_param_types(&desc);
    if types.len() != crate::count_args(&desc) {
        return None;
    }
    let cap_kinds: Vec<u8> = types
        .iter()
        .map(|&b| match b {
            b'J' => 1u8,
            b'F' => 2,
            b'D' => 3,
            b'L' | b'[' => 4,
            _ => 0,
        })
        .collect();
    // Interned through the heap's class-name pool exactly as `opcodes::indy` does, so the
    // `Arc<str>`s this site publishes are the shared ones every other reader already sees.
    let site = std::sync::Arc::new(crate::LambdaSite::new(
        vm.heap.intern_class(&impl_owner),
        vm.heap.intern_class(&impl_name),
        vm.heap.intern_class(&impl_desc),
    ));
    let site_idx = crate::nextgen::lambda_site_intern(&site);
    let cid = crate::nextgen::lambdas::lambda_class_id(&cap_kinds);
    Some(LamFacts { kind, impl_owner, impl_name, impl_desc, cap_kinds, cid, site_idx })
}

/// Which local slot each declared position of `descriptor` lands in, receiver included when
/// `!is_static` — the same arithmetic `emit_inline` performs, so a position proven here names the
/// slot the spliced body will read.
pub(crate) fn param_slots(descriptor: &str, is_static: bool) -> Vec<usize> {
    let widths = jit::arg_slot_widths(descriptor);
    let mut out = Vec::with_capacity(widths.len());
    let mut s = 0usize;
    let start = usize::from(is_static); // widths[0] is the receiver's slot
    for w in widths.iter().skip(start) {
        out.push(s);
        s += *w;
    }
    out
}

/// Find the ONE `invokeinterface` in `bc` whose RECEIVER is local slot `slot`, and return its
/// (pc, cp). `None` unless exactly one interface site in the whole body reads that slot as a
/// receiver and the body never stores to it.
///
/// ⚠**"EXACTLY ONE" IS NOT TIDINESS.** The binding this feeds is keyed by the callee's cp index,
/// so a body with two interface sites sharing one Methodref (`DataResult$Success.flatMap` calls
/// `Function.apply` and then `DataResult.addLifecycle`) must not have a plan built from one of
/// them applied to the other. Requiring uniqueness makes the cp key exact by construction.
pub(crate) fn sole_iface_site_on_slot(
    bc: &[u8],
    pool: &ConstantPool,
    max_locals: usize,
    slot: usize,
) -> Option<(usize, u16)> {
    let mut found: Option<(usize, u16)> = None;
    let mut cp_seen: std::collections::HashMap<u16, usize> = std::collections::HashMap::new();
    let leaders: std::collections::HashSet<usize> = jit::find_leaders(bc)?.into_iter().collect();
    let mut st: Vec<bool> = Vec::new(); // "this operand is an aload of `slot`"
    let mut pc = 0usize;
    while pc < bc.len() {
        if pc != 0 && leaders.contains(&pc) {
            if !st.is_empty() {
                return None;
            }
        }
        let op = bc[pc];
        let len = jit::insn_len_at(bc, pc)?;
        // A store to the tracked slot destroys the identity the caller proved.
        match op {
            0x3a if bc[pc + 1] as usize == slot => return None,
            0x4b..=0x4e if (op - 0x4b) as usize == slot => return None,
            0x36..=0x39 if bc[pc + 1] as usize == slot => return None,
            0x3b..=0x4a if ((op as usize - 0x3b) % 4) == slot => return None,
            _ => {}
        }
        match op {
            0x19 => st.push(bc[pc + 1] as usize == slot),
            0x2a..=0x2d => st.push((op - 0x2a) as usize == slot),
            0xb6..=0xb9 => {
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                let m = pool.member_ref(idx)?;
                let desc = m.descriptor.to_string();
                let nargs = crate::count_args(&desc);
                let recv = usize::from(op != 0xb8);
                if st.len() < nargs + recv {
                    return None;
                }
                let base = st.len() - nargs - recv;
                if op == 0xb9 && recv == 1 && st[base] {
                    if found.is_some() {
                        return None; // two receivers off the same slot — refuse rather than pick
                    }
                    found = Some((pc, idx));
                }
                *cp_seen.entry(idx).or_insert(0) += 1;
                st.truncate(base);
                if jit::ret_jt(&desc).is_some() {
                    st.push(false);
                }
            }
            _ => {
                // Everything else: model only the DEPTH, and only for the shapes a spliceable
                // callee body can contain. Anything else stops the walk.
                let (pops, push) = generic_effect(op, bc, pc, pool)?;
                if st.len() < pops {
                    return None;
                }
                st.truncate(st.len() - pops);
                if push {
                    st.push(false);
                }
            }
        }
        pc += len;
    }
    let (fpc, fcp) = found?;
    // The cp key must name this site alone inside this body.
    if cp_seen.get(&fcp).copied().unwrap_or(0) != 1 {
        return None;
    }
    let _ = max_locals;
    Some((fpc, fcp))
}

/// Why a SAM plan was refused. The two callers map these onto their own census rows, so the
/// admission rules live in ONE place while the profiled and the provenance populations stay
/// separately counted.
#[cfg(feature = "next-gen-gc")]
pub(crate) enum SamRefusal {
    /// `invoke_lambda_parts` binds kinds 6/7 statically; 5/8/9 dispatch or allocate.
    Kind,
    /// The two descriptors disagree on the value handed BACK.
    Ret,
    /// `ncaps + samArgs != positions` — the splice identity itself.
    Arity,
    /// A parameter position types differently on the two sides.
    CapTypes,
    /// A reference capture under `RAVA_PINNED_PTRS`: an untagged cell may be a pinned mirror.
    Pinned,
    /// The implementation is one `force_native_collection` answers from native state.
    NativeCollection,
    /// The body itself failed the ordinary inline gates.
    Body,
}

#[cfg(feature = "next-gen-gc")]
impl Vm {
    /// ⭐**THE ONE PLACE A LAMBDA SAM PLAN IS BUILT** — extracted verbatim from the profiled SAM
    /// chain in `vm/inline_targets.rs` so the provenance path cannot drift from it.
    ///
    /// `cid`/`site_idx` are the two facts the emitted guard compares; where they came from is not
    /// this function's business, and it is exactly why the guard exists: a wrong pair costs the
    /// compares and takes the site's ordinary dispatch.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sam_inline_callee(
        &mut self,
        caller: &std::sync::Arc<LoadedClass>,
        sam_desc: &str,
        impl_owner: &str,
        impl_name: &str,
        impl_desc: &str,
        kind: u8,
        n_caps: usize,
        cid: u32,
        site_idx: u32,
        static_prov: bool,
        depth: u8,
        max_frame: usize,
    ) -> Result<jit::InlineCallee, SamRefusal> {
        // (1) Kind 6 / 7 only. `invoke_lambda_parts` binds those two statically; kinds 5 and 9
        // dispatch VIRTUALLY on `call_args[0]` and kind 8 allocates and runs `<init>`.
        if !matches!(kind, 6 | 7) {
            return Err(SamRefusal::Kind);
        }
        let want_static = kind == 6;
        // (2a) The value handed back: the interpreter pushes the IMPLEMENTATION's return value
        // verbatim, and the caller's static stack shape was built from the SAM descriptor.
        if jit::ret_jt(sam_desc) != jit::ret_jt(impl_desc) {
            return Err(SamRefusal::Ret);
        }
        // (2b) The values handed in. Position 0 of an instance implementation is its receiver,
        // which no descriptor mentions.
        let mut impl_jts: Vec<jit::JT> = Vec::new();
        if !want_static {
            impl_jts.push(jit::JT::Ref);
        }
        impl_jts.extend(jit::descriptor_param_jts(impl_desc));
        let sam_jts = jit::descriptor_param_jts(sam_desc);
        // (3) captures then SAM arguments, and nothing else.
        if n_caps + sam_jts.len() != impl_jts.len() {
            return Err(SamRefusal::Arity);
        }
        if impl_jts[n_caps..] != sam_jts[..] {
            return Err(SamRefusal::CapTypes);
        }
        let cap_is_ref: Vec<bool> =
            impl_jts[..n_caps].iter().map(|t| *t == jit::JT::Ref).collect();
        // (4) With pinned pointers on, an UNTAGGED ref cell can be a pinned mirror address, so
        // the `cell & 3 == 0` test the guard emits would admit a word the interpreter reads as a
        // handle.
        if cap_is_ref.iter().any(|&r| r) && crate::nextgen::pinned_ptrs_enabled() {
            return Err(SamRefusal::Pinned);
        }
        // (5) `invoke_lambda_parts` consults `force_native_collection` before it resolves; a
        // splice that skips a question the original asked is how the two diverge.
        if crate::opcodes::force_native_collection(self, impl_owner, impl_name, impl_desc) {
            return Err(SamRefusal::NativeCollection);
        }
        let Some(mut callee) = self.inline_target_for_exact_opts(
            caller, impl_owner, impl_name, impl_desc, want_static, false, depth, max_frame, None,
            true, true, true,
        ) else {
            return Err(SamRefusal::Body);
        };
        callee.guard_class = Some(cid);
        // ⚠**THE SITE PUSHES A RECEIVER — THE CLOSURE — WHATEVER THE IMPLEMENTATION IS.**
        callee.is_static = false;
        callee.sam = Some(jit::SamInline {
            site_idx,
            cap_is_ref,
            sam_nargs: sam_jts.len(),
            impl_is_static: want_static,
            static_prov,
        });
        Ok(callee)
    }
}

/// `RAVA_CTX_LOG=1`, off by default: print each call that received a proven closure and was NOT
/// spliced. A count says the population dies one level up; only the name says whether that level
/// is a gate worth moving.
#[cfg(feature = "next-gen-gc")]
fn ctx_log_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_CTX_LOG").is_some())
}

/// ⛔⭐**OPT-IN (`RAVA_CTXINLINE=1`), BECAUSE THE POPULATION WAS MEASURED AND IT IS ZERO.**
/// `RAVA_NO_CTXINLINE=1` (and the block-level `RAVA_NO_EMIT_MORE`) force it off even when the
/// opt-in is set, so one same-binary A/B can price the round as one arm either way. Consulted
/// where the PLAN is built and nowhere else — the discipline `lambda_inline_chain_enabled`
/// documents: with it off no plan exists, so every emitter sees exactly the world it saw before
/// and the historic emission is restored EXACTLY.
///
/// The mechanism below WORKS — on a warm `ReloadBench` it proves **4,375** locally minted
/// closures and finds **1,503** of them handed straight into a call in the same basic block — and
/// it still admits nothing, for two reasons the census now states as numbers rather than as
/// guesses:
///
///  * **SHAPE A IS EMPTY: `tok.consumed-as-receiver = 0` of 4,375.** A body never APPLIES the
///    closure it mints; it hands it to somebody else. So static provenance cannot answer
///    `sam.no-profile` (17,335 refusals) at all — that population's receivers arrive as
///    parameters and captures, which no caller-local dataflow can name.
///  * **SHAPE B DIES ONE LEVEL UP: 1,432 of the 1,503 go into a call the inliner does not
///    splice**, so there is no spliced body whose interior site could be re-decided. Named by
///    `RAVA_CTX_LOG`: 414 `IllegalStateException.<init>` (a constructor — refused by rule), **294
///    `DataResult.flatMap` WITH a receiver profile already present**, 194 `Schema.register`,
///    185 `Map`/`ConcurrentMap.computeIfAbsent` (native-served), 35 `DataResult.map`, then a tail
///    of `Optional` / `Stream` / `Either` / brigadier.
///
/// ⭐**AND THE 294 NAME THE REAL BLOCKER, WHICH IS NOT A PROFILE.** `DataResult$Success.flatMap`
/// is refused by `iface_partial_gc_safe`'s `real_calls != 1`: its body makes TWO interface calls
/// and reads `this.lifecycle` BETWEEN them. That rule is the splice's liveness proof — a spliced
/// callee's locals are fresh Cranelift `Variable`s written from offset 0 of the CALLER's frame
/// buffer, so nothing of the callee is rooted across a surviving GC-capable call — not a budget
/// to widen. `DataResult$Success.map` is refused for a second, unrelated reason (`shape.new`: it
/// allocates its result). So the door context-sensitive inlining needs is held shut by the two
/// gates on the level ABOVE it, and provenance cannot open either.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_CTXINLINE").is_some()
            && std::env::var_os("RAVA_NO_CTXINLINE").is_none()
            && !crate::flags::emit_more_disabled()
    })
}

#[cfg(feature = "next-gen-gc")]
impl Vm {
    /// Both provenance shapes, applied to a top-level compile's inline map.
    ///
    /// **SHAPE A — the caller's own SAM site.** A `0xb9` whose RECEIVER this body minted is a
    /// closure call whose implementation is a constant of the instruction stream. This is the
    /// answer to `sam.no-profile`, the largest single refusal in the census, and it needs no
    /// table: `lambda_site_mono` is asked nothing.
    ///
    /// **SHAPE B — CONTEXT-SENSITIVE INLINING, the thing HotSpot does that this engine could
    /// not.** The site inside `DataResult$Success.mapOrElse` is megamorphic: one `Function.apply`
    /// Methodref serves every codec lambda in the program, so no profile keyed on it can ever
    /// name an implementation. The SAME site inside a COPY of that body spliced into ONE caller
    /// is monomorphic, because that caller passes exactly one closure, minted by one
    /// `invokedynamic` in its own bytecode. C2 reaches this state by inlining first and seeing a
    /// constant receiver second; here the caller's provenance is read at ADMISSION and the
    /// implementation is planted in the spliced copy's own nested map, which is the same fact in
    /// the other order.
    ///
    /// ⚠**THE PLANT IS KEYED BY THE CALLEE'S cp AND THAT IS SOUND BECAUSE OF THE GUARD, NOT
    /// BECAUSE OF UNIQUENESS.** If the same cp is invoked at another pc with a different closure,
    /// the emitted `AL_SITE_SLOT` compare simply misses and that execution takes the interface
    /// dispatch it always took. Provenance chooses WHICH implementation to speculate on; the
    /// guard is what makes speculating safe.
    pub(crate) fn apply_lambda_provenance(
        &mut self,
        code: &Code,
        class: &std::sync::Arc<LoadedClass>,
        out: &mut std::collections::HashMap<u16, jit::InlineCallee>,
        depth: u8,
        max_frame: usize,
    ) {
        use crate::vm::inline_census as cen;
        if !enabled() || !crate::jit::lambda_inline_chain_enabled() {
            return;
        }
        let sites = scan_sites(code, &class.file.pool);
        if sites.is_empty() {
            return;
        }
        // Deterministic order: a `HashMap` iteration order would make WHICH pc's provenance wins
        // a cp depend on the hasher, and this plan is baked into machine code.
        let mut pcs: Vec<usize> = sites.keys().copied().collect();
        pcs.sort_unstable();

        // ── SHAPE A ──────────────────────────────────────────────────────────────────────────
        for &pc in &pcs {
            let so = &sites[&pc];
            if so.op != 0xb9 || out.contains_key(&so.cp) {
                continue;
            }
            let Some(indy_cp) = so.positions.first().copied().flatten() else { continue };
            cen::note(cen::P_SAM_RECV);
            let Some(sam_desc) =
                class.file.pool.member_ref(so.cp).map(|m| m.descriptor.to_string())
            else {
                continue;
            };
            let Some(f) = lambda_facts(self, class, indy_cp) else {
                cen::note(cen::X_PROV_NOTLMF);
                continue;
            };
            let n_caps = f.cap_kinds.len();
            match self.sam_inline_callee(
                class, &sam_desc, &f.impl_owner, &f.impl_name, &f.impl_desc, f.kind, n_caps,
                f.cid, f.site_idx, true, depth, max_frame,
            ) {
                Ok(callee) => {
                    cen::note(cen::A_PROV_SAM);
                    out.insert(so.cp, callee);
                }
                Err(SamRefusal::Kind) => cen::note(cen::X_PROV_KIND),
                Err(SamRefusal::Ret | SamRefusal::Arity | SamRefusal::CapTypes) => {
                    cen::note(cen::X_PROV_DESC)
                }
                Err(_) => cen::note(cen::X_PROV_BODY),
            }
        }

        // ── SHAPE B ──────────────────────────────────────────────────────────────────────────
        // Collected first, applied second: the plan reads `out` and then writes into it.
        struct Plant {
            site_cp: u16,
            sam_cp: u16,
            callee: jit::InlineCallee,
        }
        let mut plants: Vec<Plant> = Vec::new();
        for &pc in &pcs {
            let so = &sites[&pc];
            // ⚠**`invokestatic` IS IN THE POPULATION AND HAS NO RECEIVER**, so its argument 0 is
            // `positions[0]`, not `positions[1]`. Skipping it was a counting bug in the first
            // draft of this pass and it hid the shape entirely.
            let first_arg = usize::from(so.op != 0xb8);
            if so.positions.len() <= first_arg {
                continue;
            }
            // Only a GUARDED entry may carry a surviving `invokeinterface` in its spliced body
            // (`allow_iface_body` is passed by the guarded chains alone), so only a guarded entry
            // can have an interior site to bind. Reading the descriptor and staticness here also
            // ends the borrow of `out` before the `&mut self` calls below.
            let any_arg = so.positions[first_arg..].iter().any(|t| t.is_some());
            let Some(entry) = out.get(&so.cp) else {
                if any_arg {
                    cen::note(cen::D_ARG_NOT_INLINED);
                    cen::note(match so.op {
                        0xb9 => cen::D_ARG_NI_IFACE,
                        0xb6 => cen::D_ARG_NI_VIRT,
                        0xb7 => cen::D_ARG_NI_SPECIAL,
                        _ => cen::D_ARG_NI_STATIC,
                    });
                    // Does the receiver profile the devirt chain asks for exist at this site at
                    // all? That is the difference between "the profile said megamorphic" and
                    // "the body was refused after the profile named a class".
                    let mono = matches!(so.op, 0xb6 | 0xb9)
                        && crate::sitemono::mono_class(
                            std::sync::Arc::as_ptr(class) as usize,
                            so.cp,
                        )
                        .is_some();
                    if mono {
                        cen::note(cen::D_ARG_NI_HASMONO);
                    }
                    // `RAVA_CTX_LOG=1`, off by default: WHICH calls these are. A count says the
                    // enclosing call is not inlined; only the name says whether that is a gate
                    // worth moving or a constructor that never could be.
                    if ctx_log_enabled() {
                        if let Some(m) = class.file.pool.member_ref(so.cp) {
                            eprintln!(
                                "[CTX-BLOCK] op={:#04x} mono={} {}.{}{}",
                                so.op, mono as u8, m.owner, m.name, m.descriptor
                            );
                        }
                    }
                }
                continue;
            };
            if entry.guard_class.is_none() && entry.guard_ref_word.is_none() {
                if any_arg {
                    cen::note(cen::D_ARG_UNGUARDED);
                }
                continue;
            }
            if entry.sam.is_some() {
                continue; // already a closure site — shape A owns it
            }
            let slots = param_slots(&entry.descriptor, entry.is_static);
            for (j, tok) in so.positions.iter().enumerate().skip(first_arg) {
                let Some(indy_cp) = *tok else { continue };
                let Some(&slot) = slots.get(j) else { continue };
                cen::note(cen::P_CTX_ARG);
                let entry = &out[&so.cp];
                let found = sole_iface_site_on_slot(
                    &entry.bytecode,
                    &entry.class.file.pool,
                    entry.max_locals as usize,
                    slot,
                );
                let Some((_sam_pc, sam_cp)) = found else {
                    cen::note(cen::X_CTX_NOSITE);
                    continue;
                };
                if entry.nested.contains_key(&sam_cp) {
                    cen::note(cen::X_CTX_TAKEN);
                    continue;
                }
                let Some(sam_desc) =
                    entry.class.file.pool.member_ref(sam_cp).map(|m| m.descriptor.to_string())
                else {
                    continue;
                };
                let callee_class = std::sync::Arc::clone(&entry.class);
                let Some(f) = lambda_facts(self, class, indy_cp) else {
                    cen::note(cen::X_PROV_NOTLMF);
                    continue;
                };
                let n_caps = f.cap_kinds.len();
                // The implementation is spliced INSIDE the callee, so it is admitted one level
                // deeper and resolved against the CALLEE's class (its cp indices, its pool).
                match self.sam_inline_callee(
                    &callee_class, &sam_desc, &f.impl_owner, &f.impl_name, &f.impl_desc, f.kind,
                    n_caps, f.cid, f.site_idx, true, depth + 1, max_frame,
                ) {
                    Ok(imp) => {
                        cen::note(cen::A_CTX);
                        plants.push(Plant { site_cp: so.cp, sam_cp, callee: imp });
                    }
                    Err(SamRefusal::Kind) => cen::note(cen::X_PROV_KIND),
                    Err(SamRefusal::Ret | SamRefusal::Arity | SamRefusal::CapTypes) => {
                        cen::note(cen::X_PROV_DESC)
                    }
                    Err(_) => cen::note(cen::X_PROV_BODY),
                }
            }
        }
        for p in plants {
            if let Some(e) = out.get_mut(&p.site_cp) {
                e.nested.entry(p.sam_cp).or_insert(p.callee);
            }
        }
    }
}

/// (pops, pushes-one) for the opcodes a spliceable callee body may contain, with ONE entry per
/// value (a wide value counts as one — the same convention the emitter's operand stack uses).
/// `None` = unmodelled, and every caller treats that as "stop".
fn generic_effect(op: u8, bc: &[u8], pc: usize, pool: &ConstantPool) -> Option<(usize, bool)> {
    Some(match op {
        0x00 | 0x84 | 0xa7 | 0xb1 | 0xc0 | 0xc8 => (0, false),
        0x01..=0x11 | 0x12..=0x14 | 0x15..=0x18 | 0x1a..=0x29 | 0xb2 | 0xbb => (0, true),
        0x2e..=0x35 | 0x60..=0x73 | 0x78..=0x83 | 0x94..=0x98 => (2, true),
        0x36..=0x39 | 0x3b..=0x4a | 0x57 | 0x99..=0x9e | 0xc6 | 0xc7 | 0xaa | 0xab
        | 0xac..=0xb0 | 0xb3 | 0xc2 | 0xc3 => (1, false),
        0x3a | 0x4b..=0x4e => (1, false),
        0x58 | 0x9f..=0xa6 | 0xb5 | 0x4f..=0x56 => {
            if matches!(op, 0x4f..=0x56) {
                (3, false)
            } else {
                (2, false)
            }
        }
        0x74..=0x77 | 0x85..=0x93 | 0xb4 | 0xbc | 0xbd | 0xbe | 0xc1 => (1, true),
        0xba => {
            let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
            let (_, _, desc) = pool.invoke_dynamic(idx)?;
            let desc = desc.to_string();
            (crate::count_args(&desc), jit::ret_jt(&desc).is_some())
        }
        _ => return None,
    })
}
