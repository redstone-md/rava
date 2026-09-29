//! Generic offloadability analysis — the RULE-CLEAN trigger for parallel worldgen.
//!
//! A method's full call-subtree may run on a worker thread, GIL-FREE and in
//! parallel, ONLY if it has no side effect another thread could observe: it may
//! READ immutable state and allocate fresh objects, but must NOT mutate anything
//! visible elsewhere, and every method it calls must ALSO be offloadable
//! (transitive). We decide this by the code's SEMANTICS (purity), never by a
//! hardcoded MC class/method name — so it stays within the no-adapt rule and
//! generalises to any jar.
//!
//! INCREMENT 2 (this file): an ESCAPE ANALYSIS lifts increment 1's blanket
//! putfield/array-store rejection. It abstract-interprets the operand stack +
//! locals tracking each slot's FRESHNESS (does it hold an object this call
//! allocated, still private/unpublished?). A `putfield`/array-store is permitted
//! iff its TARGET object is fresh. SOUNDNESS: a fresh object can be PUBLISHED to
//! shared state only by a `putfield`/`putstatic`/array-store whose TARGET is a
//! SHARED object — which this same rule rejects (target not fresh) / always
//! rejects (putstatic). So a fresh object never becomes reachable from shared
//! state; it can only leave via the RETURN value (owned by the fork/merge driver,
//! which publishes results under a barrier) or into a transitively-offloadable
//! callee (which, by the same analysis, won't publish it either). A constructor's
//! `this` (local 0) is treated as fresh: the object is under construction and not
//! yet published, so its field writes are private. Control-flow join points reset
//! freshness to the conservative bottom (non-fresh) — a sound under-approximation
//! of the must-be-fresh-on-all-paths merge. Anything the model can't prove (an
//! unmodelled opcode, a stack underflow, an exception handler) rejects.
//!
//! INCREMENT 3 (GAP-A01 drill): two shapes the real blocker chain hits are now
//! modelled instead of blanket-rejected. (1) An `invokedynamic` whose bootstrap
//! is `StringConcatFactory.makeConcatWithConstants` (javac's `+` operator) is a
//! PURE-ALLOC expression — rava itself serves the linked call site (opcodes.rs
//! `Plan::Concat` / the JIT `ConcatShape`), reading the popped arguments and
//! building ONE fresh String — with the side condition that every REFERENCE-typed
//! dynamic argument's declared type passes the standard callee proof for its
//! `toString()` (the serving implementation stringifies reference args through
//! `Vm::value_to_string` → the guest's real `toString()` bytecode). (2) `athrow`
//! of a FRESH throwable is PATH-TERMINAL like a return: the method has no
//! exception handlers (enforced at entry), so the throw unwinds straight out
//! publishing nothing. Both need the class's `BootstrapMethods` table, threaded
//! in as `Option<&[BootstrapMethod]>` (callers without it keep the old rejects).
//!
//! INCREMENT 4 (GAP-A01 drill iter 3): an exact `CalleeQuery` now carries
//! `recv_private` (the receiver is a still-fresh `new C` or the ctor's/fork's
//! owned `this`), so the VM-side implementation can admit the natively-served
//! Throwable-family `<init>` through its RECV_MUT contract — the pure
//! `receiver_mut_contract` table cannot key it (the class name is arbitrary);
//! the sound key needs the live loader (no-bytecode + unloadable + strict
//! Throwable-subtype proof, see `Vm::private_throwable_ctor_admissible`).
//!
//! Still DORMANT + unit-tested: no hot path calls this yet; the fork codegen
//! (parallel forEach over an offloadable per-element method) is the next step.

use rava_classfile::{BootstrapMethod, Code, ConstantPool};
#[cfg(test)]
mod effect_summary;

/// GAP-S06: one callee admissibility query. `exact == true` means `owner` IS the
/// exact dispatch target class: an `invokestatic`/`invokespecial` owner, a fresh
/// `new C` receiver's class, or the fork's concrete `this` type. `exact == false`
/// means `owner` is only the DECLARED owner of a virtual/interface call on a
/// receiver whose exact dynamic type is unknown — the implementation must prove
/// the full dispatch closure over the loaded hierarchy or reject.
#[derive(Clone, Copy)]
pub(crate) struct CalleeQuery<'a> {
    pub(crate) owner: &'a str,
    pub(crate) name: &'a str,
    pub(crate) desc: &'a str,
    pub(crate) exact: bool,
    /// GAP-A01 (drill iter 3): the RECEIVER of this callee is PRIVATE to the
    /// analysed method — a still-fresh `new C`, or the ctor's/fork-element's
    /// owned `this`. Lets the implementation admit RECEIVER-MUTATING native
    /// contracts (the natively-served Throwable `<init>`) that would be unsound
    /// on a shared receiver. Always false for static callees and for closure
    /// (shared-receiver) queries.
    pub(crate) recv_private: bool,
}

impl<'a> CalleeQuery<'a> {
    pub(crate) fn exact(owner: &'a str, name: &'a str, desc: &'a str) -> Self {
        CalleeQuery {
            owner,
            name,
            desc,
            exact: true,
            recv_private: false,
        }
    }
    /// An exact query whose receiver is proven private (fresh/owned `this`).
    pub(crate) fn exact_private(owner: &'a str, name: &'a str, desc: &'a str) -> Self {
        CalleeQuery {
            owner,
            name,
            desc,
            exact: true,
            recv_private: true,
        }
    }
    pub(crate) fn closure(owner: &'a str, name: &'a str, desc: &'a str) -> Self {
        CalleeQuery {
            owner,
            name,
            desc,
            exact: false,
            recv_private: false,
        }
    }
}

/// Why the escape analysis rejected a method (diagnostic — RAVA_OFFLOAD_PROBE).
pub(crate) enum OffReject {
    /// Exception handler present.
    Handler,
    /// Malformed bytecode / stack underflow / unanalysable control flow.
    Malformed,
    /// A forbidden or unmodelled opcode at `pc` (putstatic/non-concat indy/
    /// shared-athrow/monitor/jsr/wide/dup_x2/… — the hard offload blockers even
    /// under the relaxed ceiling).
    Opcode(usize, u8),
    /// A putfield/array-store into a NON-fresh (shared) target at `pc`.
    SharedWrite(usize, u8),
    /// FieldTxn (write-only transaction) mode: a shared READ — getstatic, a
    /// getfield/array-load on a non-fresh receiver — at `pc`. The txn runtime
    /// journals writes only; it cannot intercept reads of shared state.
    SharedRead(usize, u8),
    /// FieldTxn mode: the analysed method is a `<init>` — constructor bodies are
    /// spliced by the runtime, never journal-replayed, so they are never a txn
    /// bytecode unit.
    Ctor,
    /// A transitively non-offloadable invoke target.
    Callee(String),
}
impl std::fmt::Display for OffReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OffReject::Handler => write!(f, "exception-handler"),
            OffReject::Malformed => write!(f, "malformed/underflow"),
            OffReject::Opcode(pc, op) => write!(f, "op 0x{op:02x}@{pc}"),
            OffReject::SharedWrite(pc, op) => write!(f, "shared-write 0x{op:02x}@{pc}"),
            OffReject::SharedRead(pc, op) => write!(f, "shared-read 0x{op:02x}@{pc}"),
            OffReject::Ctor => write!(f, "constructor-body"),
            OffReject::Callee(c) => write!(f, "callee {c}"),
        }
    }
}

/// Per-method escape-analysis check: does `code` avoid PUBLISHING a mutation to
/// shared state, and is every method it invokes offloadable? `is_init` = the
/// method is a `<init>` (its `this`/local-0 is the fresh object under
/// construction). `relaxed` = the upper-bound probe: skip the freshness check on
/// writes (assume every target fresh) to measure the parallelisable ceiling.
/// Pure over the bytecode (no VM) so it is unit-testable with a mock resolver.
/// `bsm` = the declaring class's `BootstrapMethods` table (needed to model a
/// string-concat `invokedynamic`; `None` keeps the blanket indy reject).
pub(crate) fn method_offloadable_given(
    code: &Code,
    pool: &ConstantPool,
    bsm: Option<&[BootstrapMethod]>,
    relaxed: bool,
    is_init: bool,
    callee_offloadable: &mut dyn FnMut(CalleeQuery) -> bool,
) -> bool {
    analyze(
        code,
        pool,
        bsm,
        relaxed,
        false,
        is_init,
        false,
        None,
        None,
        callee_offloadable,
    )
    .is_ok()
}

/// ELEMENT-OWNERSHIP offloadability (M3 fork context): analyse a per-element method with its
/// receiver `this` (local 0) treated as WRITABLE-PRIVATE — writes to `this`'s fields are
/// permitted (like a constructor's fresh `this`). Sound ONLY inside a for-each fork where each
/// iteration owns a DISTINCT element (disjoint receiver writes) and the results are published
/// under a post-region barrier (no concurrent read of an element's fields mid-region). This is
/// what makes `BlockStateBase.initCache()`-style methods (many putfields to a published `this`)
/// forkable — the loop, not freshness, supplies the disjointness.
/// `self_class` = the concrete class of the per-element receiver (the element's runtime type at
/// fork setup), enabling `this`-call devirtualisation inside the body.
#[allow(dead_code)]
pub(crate) fn method_offloadable_forked(
    code: &Code,
    pool: &ConstantPool,
    bsm: Option<&[BootstrapMethod]>,
    relaxed: bool,
    self_class: Option<&str>,
    callee_offloadable: &mut dyn FnMut(CalleeQuery) -> bool,
) -> bool {
    analyze(
        code,
        pool,
        bsm,
        relaxed,
        false,
        false,
        true,
        self_class,
        None,
        callee_offloadable,
    )
    .is_ok()
}

/// Same analysis, returning the FIRST reject reason (RAVA_OFFLOAD_PROBE diagnostic:
/// name why a hot method can't be offloaded).
pub(crate) fn method_offloadable_reason(
    code: &Code,
    pool: &ConstantPool,
    bsm: Option<&[BootstrapMethod]>,
    relaxed: bool,
    is_init: bool,
    callee_offloadable: &mut dyn FnMut(CalleeQuery) -> bool,
) -> Option<OffReject> {
    analyze(
        code,
        pool,
        bsm,
        relaxed,
        false,
        is_init,
        false,
        None,
        None,
        callee_offloadable,
    )
    .err()
}

/// Element-ownership reason (mirrors `method_offloadable_forked`): the first reject when `this`
/// is writable-private + `this`-calls devirtualise against `self_class`.
#[allow(dead_code)]
pub(crate) fn method_offloadable_reason_forked(
    code: &Code,
    pool: &ConstantPool,
    bsm: Option<&[BootstrapMethod]>,
    relaxed: bool,
    self_class: Option<&str>,
    callee_offloadable: &mut dyn FnMut(CalleeQuery) -> bool,
) -> Option<OffReject> {
    analyze(
        code,
        pool,
        bsm,
        relaxed,
        false,
        false,
        true,
        self_class,
        None,
        callee_offloadable,
    )
    .err()
}

/// FIELD-TXN (write-only transaction) proof — a DISTINCT transactional-admission
/// question, not the purity gate: may this method's bytecode run inside an
/// authorized FieldTxn where the runtime JOURNALS field writes for later
/// (committed or aborted) replay? The txn runtime intercepts WRITES only — it
/// has NO dynamic read barrier — so every READ of shared state is unprovable and
/// rejected:
///   * REJECTED (reads the journal cannot shadow): every `getstatic`; every
///     `getfield` whose receiver is not proven Fresh/owned-`this`; every array
///     load from a shared array; invokedynamic/native-style shapes whose
///     implementation may read shared state (every invoke is dispatched to
///     `callee_offloadable` — the VM caller must reject native/unresolved
///     no-bytecode targets there, because the receiver-mutating-native shortcut
///     is disabled in this mode);
///   * REJECTED (unjournaled writes): `putstatic`, shared array stores,
///     unmodelled opcodes, `<init>` bodies (the runtime splices constructors, it
///     does not replay them);
///   * PERMITTED (runtime journals it): a `putfield` to a SHARED instance field
///     — this is the whole point of the mode, so the shared-putfield rejection
///     is LIFTED here (fresh-target writes pass for the same reason as before);
///   * INVOKES: the same `callee` proof as the pure gate (bytecode-only,
///     recursively txn-safe) — with the native shortcut off, a shared-receiver
///     virtual call still arrives as a `CalleeQuery::closure` and the exact
///     fresh-receiver devirt still applies.
/// Pure over the bytecode; the pure analyses (`method_offloadable_given` et al.)
/// are unchanged — they pass `txn = false`.
#[allow(dead_code)]
pub(crate) fn method_field_txn_safe_given(
    code: &Code,
    pool: &ConstantPool,
    bsm: Option<&[BootstrapMethod]>,
    callee_offloadable: &mut dyn FnMut(CalleeQuery) -> bool,
) -> bool {
    analyze(
        code,
        pool,
        bsm,
        false,
        true,
        false,
        false,
        None,
        None,
        callee_offloadable,
    )
    .is_ok()
}

/// FieldTxn reason (diagnostic): the FIRST shared-read / ctor / write /
/// callee reject in write-only transaction mode.
#[allow(dead_code)]
pub(crate) fn method_field_txn_safe_reason(
    code: &Code,
    pool: &ConstantPool,
    bsm: Option<&[BootstrapMethod]>,
    callee_offloadable: &mut dyn FnMut(CalleeQuery) -> bool,
) -> Option<OffReject> {
    analyze(
        code,
        pool,
        bsm,
        false,
        true,
        false,
        false,
        None,
        None,
        callee_offloadable,
    )
    .err()
}

/// M3 T2 (step 2) — is a COUNTED-LOOP method offloadable as an index-partitioned parallel map?
/// Analyse with `iv_local` (the induction var from `match_numeric_range_loop`) tracked as `F::Iv`,
/// so an `arr[iv]` write is index-partitioned — disjoint across the loop's DISTINCT iv values
/// (ascending, step>0, guaranteed by the matcher) → safe on a shared array. Every other rule is
/// the standard escape analysis (no putstatic, shared putfield only to fresh, offloadable callees,
/// …). Analyses the whole method, so apply to a method that IS the counted loop (a fill), not one
/// wrapping the loop in unrelated shared-mutating code.
#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)]
pub(crate) fn counted_loop_offloadable(
    code: &Code,
    pool: &ConstantPool,
    bsm: Option<&[BootstrapMethod]>,
    iv_local: usize,
    callee_offloadable: &mut dyn FnMut(CalleeQuery) -> bool,
) -> bool {
    analyze(
        code,
        pool,
        bsm,
        false,
        false,
        false,
        false,
        None,
        Some(iv_local),
        callee_offloadable,
    )
    .is_ok()
}

/// Reason a counted-loop body is not offloadable (diagnostic; callees TRUSTED so the reject is
/// about the body's own shape/writes, not callee purity).
#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)]
pub(crate) fn counted_loop_offloadable_reason(
    code: &Code,
    pool: &ConstantPool,
    iv_local: usize,
) -> String {
    match analyze(
        code,
        pool,
        None,
        false,
        false,
        false,
        false,
        None,
        Some(iv_local),
        &mut |_: CalleeQuery| true,
    ) {
        Ok(()) => "ok".to_string(),
        Err(e) => e.to_string(),
    }
}

fn insn_len(bc: &[u8], pc: usize) -> Result<usize, ()> {
    crate::jit::insn_len_at(bc, pc).ok_or(())
}

fn u16be(bc: &[u8], at: usize) -> u16 {
    ((*bc.get(at).unwrap_or(&0) as u16) << 8) | *bc.get(at + 1).unwrap_or(&0) as u16
}

/// Field slot width (1, or 2 for long/double) from a Fieldref's descriptor.
fn field_width(pool: &ConstantPool, cp: u16) -> usize {
    match pool.member_ref(cp).map(|f| f.descriptor) {
        Some(d) if d.starts_with('J') || d.starts_with('D') => 2,
        _ => 1,
    }
}

/// Resolve constant-pool entry `idx` as a string-concat `invokedynamic` site:
/// an `InvokeDynamic` whose `BootstrapMethods` entry's `MethodHandle` names
/// `java/lang/invoke/StringConcatFactory.makeConcatWithConstants` and whose
/// call-site descriptor returns a String. Returns the call-site descriptor on a
/// match; `None` for anything else — including a missing/short bootstrap table
/// (no table ⇒ the site cannot be proven to be a concat ⇒ caller rejects).
/// GILFREE-RINGS: resolve cp `idx` as a LambdaMetafactory `invokedynamic` and
/// return its IMPL method `(ref_kind, owner, name, desc)` — the exact SAM target
/// the minted lambda will dispatch to (rava's own indy service links precisely
/// this handle). Derived purely from the BSM table (bytecode-proof, no-adapt).
fn lmf_indy_impl<'p>(
    pool: &'p ConstantPool,
    bsm: Option<&[BootstrapMethod]>,
    idx: u16,
) -> Option<(u8, &'p str, &'p str, &'p str)> {
    let (bootstrap, _name, _desc) = pool.invoke_dynamic(idx)?;
    let bm = bsm?.get(bootstrap as usize)?;
    let factory = pool.method_handle(bm.handle_index)?;
    if factory.owner != "java/lang/invoke/LambdaMetafactory"
        || !(factory.name == "metafactory" || factory.name == "altMetafactory")
    {
        return None;
    }
    // BSM static args: [0]=samMethodType, [1]=implMethod handle, [2]=instantiated.
    let (kind, m) = pool.method_handle_full(*bm.args.get(1)?)?;
    Some((kind, m.owner, m.name, m.descriptor))
}

fn concat_indy_desc<'p>(
    pool: &'p ConstantPool,
    bsm: Option<&[BootstrapMethod]>,
    idx: u16,
) -> Option<&'p str> {
    let (bootstrap, _name, desc) = pool.invoke_dynamic(idx)?;
    if !desc.ends_with(")Ljava/lang/String;") {
        return None;
    }
    let bm = bsm?.get(bootstrap as usize)?;
    let handle = pool.method_handle(bm.handle_index)?;
    (handle.owner == "java/lang/invoke/StringConcatFactory"
        && handle.name == "makeConcatWithConstants")
        .then_some(desc)
}

fn i32be(bc: &[u8], at: usize) -> Option<i32> {
    Some(
        ((*bc.get(at)? as i32) << 24)
            | ((*bc.get(at + 1)? as i32) << 16)
            | ((*bc.get(at + 2)? as i32) << 8)
            | (*bc.get(at + 3)? as i32),
    )
}
mod analyze;
pub(crate) use analyze::*;
mod descriptors;
pub(crate) use descriptors::*;
#[cfg(test)]
mod tests;
