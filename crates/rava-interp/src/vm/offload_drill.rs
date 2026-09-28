//! The drill-down that explains a refusal, one call-tree node at a time.

use crate::*;

impl Vm {
    /// RAVA_OFFLOAD_DRILL (read-once gate): recursive REJECTION-CHAIN diagnostic.
    pub(crate) fn offload_drill_gate() -> bool {
        static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *F.get_or_init(|| std::env::var_os("RAVA_OFFLOAD_DRILL").is_some())
    }

    /// RAVA_OFFLOAD_DRILL: print ONE indented rejection-chain tree per distinct rejected
    /// fork root (memoised globally). Diagnostic only — it runs strictly AFTER the sound
    /// gate already rejected the candidate, changes no verdict, and re-uses the exact
    /// memoised proofs (`is_offloadable_rec` / `virtual_dispatch_closure`) the gate ran.
    #[allow(dead_code)]
    pub(crate) fn offload_drill_print(&mut self, root_class: &str, name: &str, desc: &str) {
        static PRINTED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
            std::sync::OnceLock::new();
        let key = format!("{root_class}.{name}{desc}");
        {
            let set = PRINTED.get_or_init(|| std::sync::Mutex::new(Default::default()));
            if !set.lock().unwrap().insert(key.clone()) {
                return; // one tree per distinct root, not one per rejection
            }
        }
        self.offload_epoch_sync();
        let mut out = String::new();
        out.push_str(&format!("[OFFLOAD-DRILL] rejection chain for {key} (element-ownership root)\n"));
        let mut seen = std::collections::HashSet::new();
        self.offload_drill_node(root_class, name, desc, true, 1, &mut seen, &mut out);
        eprint!("{out}");
    }

    /// One drill node: report this method's first reject reason, then recurse into the
    /// failing callee ("callee X" reasons) — for a closure failure, enumerate the loaded
    /// dispatch closure and drill each FAILING target (capped). `forked` = analyse with
    /// element-ownership `this` (root only, matching the fork gate's root analysis).
    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn offload_drill_node(
        &mut self,
        class: &str,
        name: &str,
        desc: &str,
        forked: bool,
        depth: usize,
        seen: &mut std::collections::HashSet<String>,
        out: &mut String,
    ) {
        let indent = "  ".repeat(depth);
        let key = format!("{class}.{name}{desc}");
        if !seen.insert(key.clone()) {
            out.push_str(&format!("{indent}{key} -> (analysed above)\n"));
            return;
        }
        let (reason, fail) = self.offload_drill_reason(class, name, desc, forked);
        out.push_str(&format!("{indent}{key} -> {reason}\n"));
        if depth >= 8 {
            if fail.is_some() {
                out.push_str(&format!("{indent}  (depth cap)\n"));
            }
            return;
        }
        let Some((o, n, d, exact)) = fail else { return };
        if exact {
            self.offload_drill_node(&o, &n, &d, false, depth + 1, seen, out);
            return;
        }
        // Closure failure: name WHICH loaded concrete targets fail, and drill each.
        match self.virtual_dispatch_closure(&o, &n, &d) {
            None => {
                const ACC_INTERFACE: u16 = 0x0200;
                let why = match self.loader.get_or_load(&o) {
                    Ok(c) if c.file.access_flags & ACC_INTERFACE != 0 => {
                        "declared owner is an INTERFACE (implementors unenumerable)".to_string()
                    }
                    Ok(_) => "closure unprovable (owner loads, non-interface)".to_string(),
                    // GAP-A01 iter 3: name which leg of the unloadable-owner contract
                    // fallback failed (this branch prints only when the gate rejected).
                    Err(_) => {
                        let pure = matches!(
                            crate::native_contract::contract_for(&o, &n, &d, None),
                            Some(crate::native_contract::NativeContract {
                                effect: crate::native_contract::NativeEffect::Pure,
                                may_callback: false,
                                ..
                            })
                        );
                        if !pure {
                            "declared owner UNLOADABLE [no pure native contract]".to_string()
                        } else if !self.unloadable_owner_closed(&o) {
                            "declared owner UNLOADABLE [pure contract, but a loaded class possibly subtypes the owner]".to_string()
                        } else {
                            "declared owner UNLOADABLE [contract fallback admits — not the blocker]".to_string()
                        }
                    }
                };
                out.push_str(&format!("{indent}  closure {o}.{n}{d} -> UNENUMERABLE: {why}\n"));
                // GAP-A01 STAGE 1 (drill-only): whatever the refusal reason, ALSO
                // enumerate the unloadable-iface closure's Gate 1 — every loaded
                // concrete MAY-SUBTYPE implementor — and group its failing targets,
                // so the real Gate-1 population is measurable BEFORE any admission
                // is wired to it. Changes no verdict (prints only after rejection).
                let its = self.interface_dispatch_closure(&o, &n, &d);
                self.offload_drill_closure_targets(
                    &format!("iface-closure(Gate-1) {o}.{n}{d}"),
                    &its, &n, &d, depth, seen, out,
                );
            }
            Some(ts) => {
                self.offload_drill_closure_targets(
                    &format!("closure {o}.{n}{d}"),
                    &ts, &n, &d, depth, seen, out,
                );
            }
        }
    }

    /// Shared drill tail over an ENUMERATED closure: count the failing targets,
    /// GROUP them by their one-level reason, and drill ONE representative per
    /// distinct reason (complete leaf coverage, no spam). Used by both the
    /// class-owner closure arm and the stage-1 Gate-1 (iface-closure) arm.
    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn offload_drill_closure_targets(
        &mut self,
        label: &str,
        ts: &[String],
        n: &str,
        d: &str,
        depth: usize,
        seen: &mut std::collections::HashSet<String>,
        out: &mut String,
    ) {
        let indent = "  ".repeat(depth);
        let mut failing: Vec<String> = Vec::new();
        for t in ts.iter() {
            let mut ip = std::collections::HashSet::new();
            if !self.is_offloadable_rec(t, n, d, false, false, 1, &mut ip) {
                failing.push(t.clone());
            }
        }
        out.push_str(&format!(
            "{indent}  {label} -> {} loaded concrete targets, {} FAILING\n",
            ts.len(),
            failing.len()
        ));
        let mut groups: Vec<(String, Vec<String>)> = Vec::new();
        for t in &failing {
            let (reason, _) = self.offload_drill_reason(t, n, d, false);
            match groups.iter_mut().find(|(r, _)| r == &reason) {
                Some((_, v)) => v.push(t.clone()),
                None => groups.push((reason, vec![t.clone()])),
            }
        }
        for (reason, members) in &groups {
            let sample: Vec<&str> = members.iter().take(4).map(|s| s.rsplit('/').next().unwrap_or(s)).collect();
            out.push_str(&format!(
                "{indent}  [{} target(s): {}{}] -> {reason}\n",
                members.len(),
                sample.join(", "),
                if members.len() > 4 { ", ..." } else { "" }
            ));
            self.offload_drill_node(&members[0], n, d, false, depth + 1, seen, out);
        }
    }

    /// The reason analysis for one drill node, mirroring `offload_reason`/`offload_reason_devirt`
    /// but ALSO capturing the exact failing `CalleeQuery` (owner, name, desc, exact) so the drill
    /// can recurse, and enriching leaf reasons with the field/opcode evidence + native-contract
    /// status. Returns (reason text, failing callee query if the reason was a callee).
    #[allow(dead_code)]
    pub(crate) fn offload_drill_reason(
        &mut self,
        class: &str,
        name: &str,
        desc: &str,
        forked: bool,
    ) -> (String, Option<(String, String, String, bool)>) {
        let contract_note = |c: Option<crate::native_contract::NativeContract>| match c {
            Some(nc) => format!(
                "contract effect={:?} allocates={} may_callback={}",
                nc.effect, nc.allocates, nc.may_callback
            ),
            None => "NO-CONTRACT".to_string(),
        };
        let decl = match self.resolve_method(class, name, desc) {
            Ok(Some(d)) => d,
            _ => {
                let flags = self.loader.get_or_load(class).ok().map(|c| c.file.access_flags);
                let c = crate::native_contract::contract_for(class, name, desc, flags);
                // GAP-A01 iter 3: name the throwable-ctor RECV_MUT candidacy — this
                // leaf prints only when the gate rejected, i.e. the receiver was not
                // private or a VM-side condition (unloadable/strict-subtype) failed.
                let thr = if c.is_none() && self.private_throwable_ctor_admissible(class, name, desc) {
                    " [throwable-ctor RECV_MUT contract: admissible only on a PRIVATE receiver]"
                } else {
                    ""
                };
                return (format!("NATIVE/unresolved [{}]{thr}", contract_note(c)), None);
            }
        };
        let Some(code) = decl.file.method(name, desc).and_then(|m| m.code.as_ref()) else {
            let flags = self.loader.get_or_load(class).ok().map(|c| c.file.access_flags);
            let c = crate::native_contract::contract_for(class, name, desc, flags);
            return (
                format!("NO-BYTECODE (declared in {}) [{}]", decl.file.this_class, contract_note(c)),
                None,
            );
        };
        let mut in_progress = std::collections::HashSet::new();
        let mut last_fail: Option<(String, String, String, bool)> = None;
        let pool = &decl.file.pool;
        let r = {
            let cb = &mut |q: crate::offload::CalleeQuery| {
                let ok = if q.exact {
                    self.is_offloadable_rec(q.owner, q.name, q.desc, false, false, 1, &mut in_progress)
                        || (q.recv_private && self.private_throwable_ctor_admissible(q.owner, q.name, q.desc))
                } else {
                    match self.virtual_dispatch_closure(q.owner, q.name, q.desc) {
                        Some(ts) => ts
                            .iter()
                            .all(|t| self.is_offloadable_rec(t, q.name, q.desc, false, false, 1, &mut in_progress)),
                        None => self.unloadable_owner_contract_fallback(q.owner, q.name, q.desc),
                    }
                };
                if !ok {
                    last_fail = Some((q.owner.to_string(), q.name.to_string(), q.desc.to_string(), q.exact));
                }
                ok
            };
            let bsm = Some(decl.file.bootstrap_methods.as_slice());
            if forked {
                crate::offload::method_offloadable_reason_forked(code, pool, bsm, false, Some(class), cb)
            } else {
                crate::offload::method_offloadable_reason(code, pool, bsm, false, name == "<init>", cb)
            }
        };
        // Field evidence for a putfield/putstatic leaf: decode the cp operand at pc+1.
        let field_at = |pc: usize| -> String {
            let bc = &code.bytecode;
            let cp = ((*bc.get(pc + 1).unwrap_or(&0) as u16) << 8) | *bc.get(pc + 2).unwrap_or(&0) as u16;
            pool.member_ref(cp)
                .map(|m| format!(" field {}.{}:{}", m.owner, m.name, m.descriptor))
                .unwrap_or_default()
        };
        match r {
            None => ("OK (offloadable)".to_string(), None),
            Some(crate::offload::OffReject::Callee(c)) => {
                let kind = match &last_fail {
                    Some((_, _, _, true)) => "exact",
                    Some(_) => "closure",
                    None => "?",
                };
                (format!("callee {c} [{kind}]"), last_fail)
            }
            Some(crate::offload::OffReject::SharedWrite(pc, op)) => {
                let detail = if op == 0xb5 { field_at(pc) } else { String::new() };
                (format!("SHARED-WRITE 0x{op:02x}@{pc}{detail}"), None)
            }
            Some(crate::offload::OffReject::Opcode(pc, op)) => {
                let nm = match op {
                    0xb3 => " putstatic",
                    0xba => " invokedynamic",
                    0xbf => " athrow",
                    0xc2 => " monitorenter",
                    0xc3 => " monitorexit",
                    _ => "",
                };
                let detail = if op == 0xb3 { field_at(pc) } else { String::new() };
                (format!("OPCODE 0x{op:02x}@{pc}{nm}{detail}"), None)
            }
            Some(other) => (other.to_string(), None),
        }
    }
}
