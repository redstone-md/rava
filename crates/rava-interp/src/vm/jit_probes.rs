//! Read-only probes that run when a method first goes hot.
//!
//! Three gated diagnostics -- AOT translator coverage, the offload upper bound, and the
//! object-building fork shapes. None of them changes what is compiled; they exist to measure
//! what COULD be, on real bytecode, before a lever is built for it.

use crate::*;
use crate::class_loader::LoadedClass;
use rava_classfile::{Code, Constant};
use std::sync::Arc;

impl Vm {
    pub(crate) fn jit_hot_probes(&mut self, code: &Code, class: &Arc<LoadedClass>, name: &str, descriptor: &str) {
            // A4-WIRE COVERAGE PROBE (RAVA_AOT_PROBE): for each method that reaches
            // the compile threshold, try the AOT translator (compile_method) with a
            // real field_of from this class's constant pool — COMPILE-AND-DISCARD,
            // never run, so zero execution risk. Counts covered/total to measure what
            // fraction of boot methods the current translator handles (the number A5
            // needs to predict the boot-time win). Prints a running tally.
            #[cfg(feature = "next-gen-gc")]
            if std::env::var_os("RAVA_AOT_PROBE").is_some() {
                use std::sync::atomic::{AtomicU64, Ordering};
                static COVERED: AtomicU64 = AtomicU64::new(0);
                static TOTAL: AtomicU64 = AtomicU64::new(0);
                let pool = &class.file.pool;
                let field_of = |cp: u16| -> Option<(String, String, crate::aot_clif::FieldKind)> {
            use crate::aot_clif::FieldKind;
            let m = pool.member_ref(cp)?;
            let k = match m.descriptor.as_bytes().first()? {
                b'L' | b'[' => FieldKind::Ref,
                b'I' | b'Z' | b'B' | b'C' | b'S' => FieldKind::Int,
                b'J' => FieldKind::Long,
                b'F' => FieldKind::Float,
                b'D' => FieldKind::Double,
                _ => return None,
            };
            Some((m.owner.to_string(), m.name.to_string(), k))
        };
                let method_of = |cp: u16| -> Option<crate::aot_clif::InvokeInfo> {
                    let m = pool.member_ref(cp)?;
                    // ncells = receiver (non-static) + one cell per param (long/double
                    // count as one cell in rava_invoke's buffer, matching the interp).
                    let is_static_call = false; // getdescriptor kind is set per opcode below; count params
                    let nparams = crate::count_args(m.descriptor) as u32;
                    let _ = is_static_call;
                    let rb = m.descriptor.as_bytes();
                    let rstart = m.descriptor.rfind(')').map(|p| p + 1)?;
                    let ret_void = rb.get(rstart) == Some(&b'V');
                    let ret_ref = matches!(rb.get(rstart), Some(b'L') | Some(b'['));
                    let ret_wide = matches!(rb.get(rstart), Some(b'J') | Some(b'D'));
                    // Static vs instance receiver is opcode-determined; the probe can't
                    // see the opcode here, so assume 1 receiver cell for virtual/special/
                    // interface (the common case) — the probe over-counts static calls
                    // by one, which only affects the coverage %, not correctness.
                    Some(crate::aot_clif::InvokeInfo {
                        numeric_leaf: None, object_leaf: None,
                        direct_leaf: crate::aot_clif::DirectLeafCall::from_member(
                            m.owner, m.name, m.descriptor,
                        ),
                        ncells: nparams + 1,
                        kind: 1,
                        ret_void,
                        ret_ref,
                        ret_wide,
                    })
                };
                let indy_of = |cp: u16| -> Option<crate::aot_clif::InvokeInfo> {
                    let (_bs, _name, desc) = pool.invoke_dynamic(cp)?;
                    let nparams = crate::count_args(desc) as u32;
                    let rb = desc.as_bytes();
                    let rstart = desc.rfind(')').map(|p| p + 1)?;
                    let ret_void = rb.get(rstart) == Some(&b'V');
                    let ret_ref = matches!(rb.get(rstart), Some(b'L') | Some(b'['));
                    let ret_wide = matches!(rb.get(rstart), Some(b'J') | Some(b'D'));
                    // indy dynamic args only (no receiver); kind unused.
                    Some(crate::aot_clif::InvokeInfo {
                        numeric_leaf: None, object_leaf: None, direct_leaf: None,
                        ncells: nparams,
                        kind: 0,
                        ret_void,
                        ret_ref,
                        ret_wide,
                    })
                };
                let ldc_of = |cp: u16| -> Option<crate::aot_clif::LdcConst> {
                    match pool.get(cp)? {
                Constant::String { .. } => Some(crate::aot_clif::LdcConst::Str(pool.string_const(cp)?.to_string())),
                        Constant::Class { .. } => Some(crate::aot_clif::LdcConst::Class),
                Constant::Integer(v) => Some(crate::aot_clif::LdcConst::Int(*v)),
                Constant::Float(f) => Some(crate::aot_clif::LdcConst::Float(f.to_bits())),
                Constant::Long(v) => Some(crate::aot_clif::LdcConst::Long(*v)),
                Constant::Double(d) => Some(crate::aot_clif::LdcConst::Double(d.to_bits())),
                _ => None,
            }
                };
                let ok = crate::aot_clif::compile_method(
                    &code.bytecode,
                    &class.file.this_class,
                    code.max_locals as u32,
                    &field_of,
                    &method_of,
                    &indy_of,
                    &ldc_of,
                )
                .is_some();
                let t = TOTAL.fetch_add(1, Ordering::Relaxed) + 1;
                if ok {
                    COVERED.fetch_add(1, Ordering::Relaxed);
                }
                // Tally the uncovered opcodes of every FAILED method → the top
                // blockers to add next (data-driven coverage, not guessing).
                static BLOCKERS: std::sync::Mutex<Option<std::collections::HashMap<u8, u32>>> =
                    std::sync::Mutex::new(None);
                if !ok {
                    if let Ok(mut g) = BLOCKERS.lock() {
                        let m = g.get_or_insert_with(std::collections::HashMap::new);
                        for op in crate::aot_clif::uncovered_opcodes(&code.bytecode) {
                            *m.entry(op).or_insert(0) += 1;
                        }
                    }
                }
                if t % 200 == 0 {
                    eprintln!(
                        "[AOT-PROBE] translator covers {}/{} hot methods ({:.1}%)",
                        COVERED.load(Ordering::Relaxed),
                        t,
                        100.0 * COVERED.load(Ordering::Relaxed) as f64 / t as f64
                    );
                    if let Ok(g) = BLOCKERS.lock() {
                        if let Some(m) = g.as_ref() {
                            let mut v: Vec<_> = m.iter().map(|(k, n)| (*n, *k)).collect();
                            v.sort_unstable_by(|a, b| b.cmp(a));
                            let top: Vec<String> =
                                v.iter().take(10).map(|(n, op)| format!("0x{op:02x}:{n}")).collect();
                            eprintln!("[AOT-BLOCKERS] top uncovered: {}", top.join(" "));
                        }
                    }
                }
            }
            // PARALLEL-WORLDGEN #21 measurement: this method just went HOT. Probe
            // whether its whole subtree is offloadable (relaxed = UPPER BOUND:
            // assume every putfield/array-store hits a fresh object). If even the
            // upper bound is near-zero, a sound escape analysis can only do worse
            // → generic offload won't parallelize this jar. RAVA_OFFLOAD_PROBE.
            if std::env::var("RAVA_OFFLOAD_PROBE").is_ok() {
                use std::sync::atomic::{AtomicU64, Ordering};
                static SOUND: AtomicU64 = AtomicU64::new(0);
                static CEIL: AtomicU64 = AtomicU64::new(0);
                static TOT: AtomicU64 = AtomicU64::new(0);
                let cn = class.file.this_class.to_string();
                // CEIL = relaxed upper bound (assume every write fresh); SOUND =
                // the increment-2 escape analysis (only fresh-target writes). The
                // GAP (ceil-yes, sound-no) is where the escape model must improve.
                let ceil = self.is_offloadable(&cn, name, descriptor, true);
                let sound = ceil && self.is_offloadable(&cn, name, descriptor, false);
                let t = TOT.fetch_add(1, Ordering::Relaxed) + 1;
                if ceil {
                    CEIL.fetch_add(1, Ordering::Relaxed);
                }
                // ELEMENT-OWNERSHIP verdict (receiver_private=true, devirt against own
                // class): a VOID method whose only mutation is to its own receiver becomes
                // offloadable in a per-element fork even though `sound` (shared this) rejects
                // it. This is the verdict the iterator-loop fork gate will use for the
                // block-state initCache-style methods — measure it + name the residual
                // blocker so the native-callee classification can be extended.
                let owned = self.is_offloadable_devirt(&cn, name, descriptor, false, true);
                if owned && !sound {
                    let why = self.offload_reason(&cn, name, descriptor, false).unwrap_or_default();
                    eprintln!("[OFFLOAD-OWNED] {cn}.{name}{descriptor} (shared-this blocked by: {why})");
                }
                if sound {
                    SOUND.fetch_add(1, Ordering::Relaxed);
                    eprintln!("[OFFLOAD-SOUND] {cn}.{name}{descriptor}");
                } else if ceil {
                    let owned_tag = if owned { " OWNED-YES" } else { " OWNED-NO" };
                    eprintln!("[OFFLOAD-GAP]{owned_tag} ceil-yes sound-no: {cn}.{name}{descriptor}");
                } else {
                    // Even the relaxed ceiling rejects → a hard offload blocker.
                    // Name the reason (blocking opcode / non-offloadable callee).
                    let why = self.offload_reason(&cn, name, descriptor, true).unwrap_or_default();
                    eprintln!("[OFFLOAD-BLOCKED] {cn}.{name}{descriptor} — {why}");
                }
                if t % 50 == 0 {
                    eprintln!(
                        "[OFFLOAD] sound {} / ceil {} / hot {t}",
                        SOUND.load(Ordering::Relaxed),
                        CEIL.load(Ordering::Relaxed)
                    );
                }
            }
            // M3 FORK PROBE (RAVA_FORK_PROBE, next-gen-gc): the SAFE prelim before F3
            // execution. When this hot method contains an object-building for-each map
            // (F1 `analyze_offloadable_foreach`), report the per-element call site + does
            // it pass the offloadability gate as-declared, and is it a virtual/interface
            // call (→ needs F2/F3 runtime devirt against the concrete element type). This
            // measures how many shape-build loops exist + how much runtime devirt matters,
            // BEFORE committing to the concurrent-GC fork codegen. Zero runtime change.
            #[cfg(feature = "next-gen-gc")]
            if std::env::var_os("RAVA_FORK_PROBE").is_some() {
                if let Some(fe) = jit::analyze_offloadable_foreach(&code.bytecode) {
                    let (owner, mname, mdesc) = {
                        let pool = &class.file.pool;
                        match pool.member_ref(fe.call_cp) {
                            Some(m) => (m.owner.to_string(), m.name.to_string(), m.descriptor.to_string()),
                            None => (String::new(), String::new(), String::new()),
                        }
                    };
                    let cn = class.file.this_class.to_string();
                    let declared_ok = self.is_offloadable(&owner, &mname, &mdesc, false);
                    let virt = matches!(fe.call_op, 0xb6 | 0xb9);
                    eprintln!(
                        "[FORK-SHAPE] {cn}.{name}{descriptor} → per-elem {owner}.{mname}{mdesc} \
                         declared-offloadable={declared_ok} virtual={virt} needs-devirt={}",
                        virt && !declared_ok
                    );
                }
                // Diagnostic-only iterator census: the runtime fork hook may miss loops
                // that reach JIT compilation before interpretation. Reuse the existing
                // structural matcher without changing admission or execution.
                for pc in 0..code.bytecode.len().saturating_sub(4) {
                    if code.bytecode[pc] != 0xb9 {
                        continue;
                    }
                    let Some(member) = class.file.pool.member_ref(
                        u16::from_be_bytes([code.bytecode[pc + 1], code.bytecode[pc + 2]]),
                    ) else {
                        continue;
                    };
                    if member.name != "iterator" || !member.descriptor.ends_with("Iterator;") {
                        continue;
                    }
                    if let Some(m) = jit::match_iterator_foreach_split(&code.bytecode, pc) {
                        let cn = class.file.this_class.to_string();
                        let elem = class.file.pool.member_ref(m.elem_cp)
                            .map(|e| format!("{}{}", e.owner, e.descriptor))
                            .unwrap_or_default();
                        eprintln!("[FORK-ITER-SHAPE] {cn}.{name}{descriptor} pc={pc} elem={elem} prefix={}..{}", m.prefix_range.0, m.prefix_range.1);
                    }
                }
            }
    }
}
