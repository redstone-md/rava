//! Admitting a call to the pure-value memo, and why one is refused.

use crate::*;

impl Vm {
    /// PURE-CALL MEMO admission for ONE concrete dispatch `(cls, name, desc)` — see
    /// `pure_memo` for the soundness argument this predicate is one half of.
    ///
    /// `Memo`   — the transitive escape analysis proves the whole call subtree writes
    ///            nothing that existed when the call began, the return is a PRIMITIVE
    ///            (so replaying it cannot alias two allocations), the operand buffer
    ///            fits one key, and the body does enough work to be worth a host fall.
    /// `Opener` — effect-free and substantial, but its own result is not replayable;
    ///            it may still OPEN the frozen window its callees memoize inside.
    /// `No`     — everything else; stays on the machine-code fast path untouched.
    ///
    /// Derived entirely from bytecode (effects, descriptor, size, leafness): no class,
    /// method or package name is consulted anywhere.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn pure_memo_admit(&mut self, cls: &str, name: &str, desc: &str) -> crate::pure_memo::Site {
        use crate::pure_memo::{Site, Verdict};
        const NONE: Site = Site { verdict: Verdict::No, code: 0, ref_mask: 0, slots: 0 };
        let census = crate::pure_memo::probe_enabled();
        let mut record = |v: Verdict, why: &str| {
            if census {
                crate::pure_memo::admission::note(format!("{cls}.{name}{desc}"), v, why.to_string());
            }
        };
        let Ok(Some(decl)) = self.resolve_method(cls, name, desc) else {
            record(Verdict::No, "unresolved/native");
            return NONE;
        };
        let Some(method) = decl.file.method(name, desc) else {
            record(Verdict::No, "no method");
            return NONE;
        };
        let Some(code) = method.code.as_ref() else {
            record(Verdict::No, "no bytecode");
            return NONE;
        };
        // WORTH-IT gate (generic, size/shape only): a body that neither calls anything
        // nor spans a few dozen bytes cannot repay a fall into the host, and admitting
        // it would only add work to the fast path.
        let leaf = jit::is_gc_leaf(&code.bytecode);
        if leaf && code.bytecode.len() < 32 {
            record(Verdict::No, "too small to repay a host fall");
            return NONE;
        }
        // The effect proof — the property the whole window rests on.
        if !self.is_offloadable_devirt(cls, name, desc, false, false) {
            let why = if census {
                format!("not effect-free: {}", self.pure_memo_reject_reason(cls, name, desc))
            } else {
                String::new()
            };
            record(Verdict::No, &why);
            return NONE;
        }
        let code_ptr = code as *const Code as usize;
        let is_static = method.access_flags & 0x0008 != 0;
        let shape = crate::pure_memo::desc_slot_shape(desc, is_static);
        let prim = crate::pure_memo::returns_primitive(desc).unwrap_or(false);
        match (prim, shape) {
            (true, Some((ref_mask, slots))) => {
                record(Verdict::Memo, "effect-free, primitive return");
                Site { verdict: Verdict::Memo, code: code_ptr, ref_mask, slots }
            }
            _ => {
                record(Verdict::Opener, "effect-free; opens a window for its callees");
                Site { verdict: Verdict::Opener, code: code_ptr, ref_mask: 0, slots: 0 }
            }
        }
    }

    /// Census-only: the FIRST reason the escape analysis refuses `(cls, name, desc)`,
    /// followed down the callee chain (bounded) to the method that actually offends.
    /// Never consulted on a hot path — it allocates freely.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn pure_memo_reject_reason(&mut self, cls: &str, name: &str, desc: &str) -> String {
        let mut chain: Vec<String> = Vec::new();
        let (mut c, mut n, mut d) = (cls.to_string(), name.to_string(), desc.to_string());
        for _ in 0..10 {
            let Ok(Some(decl)) = self.resolve_method(&c, &n, &d) else {
                // An INTERFACE/abstract owner has no body of its own: the analysis had
                // to prove the whole dispatch closure. Name the members that failed —
                // that is the difference between "one impure implementor" and
                // "unenumerable".
                let targets = self.interface_dispatch_closure(&c, &n, &d);
                if targets.is_empty() {
                    return format!("{} -> closure of {c}.{n} is EMPTY/unenumerable", chain.join(" -> "));
                }
                let bad: Vec<String> = targets
                    .iter()
                    .filter(|t| !self.is_offloadable(t, &n, &d, false))
                    .cloned()
                    .collect();
                chain.push(format!("{c}.{n} [closure {}/{} impure]", bad.len(), targets.len()));
                let Some(first) = bad.first().cloned() else {
                    return format!("{} -> closure all-pure but still refused", chain.join(" -> "));
                };
                c = first;
                continue;
            };
            let Some(code) = decl.file.method(&n, &d).and_then(|m| m.code.as_ref()).cloned() else {
                return format!("{} -> no bytecode {c}.{n}", chain.join(" -> "));
            };
            // `decl` is an owned Arc, so borrowing its pool does NOT borrow `self` —
            // the recursion closure below can still take `&mut self`.
            let pool = &decl.file.pool;
            let bsm: Vec<_> = decl.file.bootstrap_methods.clone();
            let is_init = n == "<init>";
            let reason = {
                // Mirror `is_offloadable_rec`'s callee predicate EXACTLY (including the
                // interface-closure and unloadable-owner fallbacks); a stricter stand-in
                // would blame the wrong callee.
                let recurse = &mut |q: crate::offload::CalleeQuery| {
                    if q.exact {
                        self.is_offloadable(q.owner, q.name, q.desc, false)
                            || (q.recv_private
                                && self.private_throwable_ctor_admissible(q.owner, q.name, q.desc))
                    } else {
                        match self.virtual_dispatch_closure(q.owner, q.name, q.desc) {
                            Some(t) => t.iter().all(|t| self.is_offloadable(t, q.name, q.desc, false)),
                            None => {
                                const ACC_INTERFACE: u16 = 0x0200;
                                let is_iface = self
                                    .loader
                                    .cached(q.owner)
                                    .is_some_and(|c| c.file.access_flags & ACC_INTERFACE != 0);
                                if is_iface {
                                    let t = self.interface_dispatch_closure(q.owner, q.name, q.desc);
                                    !t.is_empty()
                                        && t.iter().all(|t| self.is_offloadable(t, q.name, q.desc, false))
                                } else {
                                    self.unloadable_owner_contract_fallback(q.owner, q.name, q.desc)
                                }
                            }
                        }
                    }
                };
                crate::offload::method_offloadable_reason(
                    &code,
                    pool,
                    Some(bsm.as_slice()),
                    false,
                    is_init,
                    recurse,
                )
            };
            match reason {
                None => return format!("{} -> (no reject at top)", chain.join(" -> ")),
                Some(crate::offload::OffReject::Callee(target)) => {
                    chain.push(format!("{c}.{n}"));
                    // "owner.name(desc)ret" — split the descriptor off, then the owner.
                    let parsed = target
                        .split_once('(')
                        .and_then(|(head, tail)| head.rsplit_once('.').map(|(o, nm)| (o.to_string(), nm.to_string(), format!("({tail}"))));
                    let Some((owner, nm, dd)) = parsed else {
                        return format!("{} -> callee {target}", chain.join(" -> "));
                    };
                    c = owner;
                    n = nm;
                    d = dd;
                }
                Some(other) => {
                    chain.push(format!("{c}.{n}"));
                    return format!("{} :: {other}", chain.join(" -> "));
                }
            }
        }
        format!("{} -> (depth cap)", chain.join(" -> "))
    }

    /// Is `cell` admissible as part of a memo key? A reference is only stable across a
    /// window when it cannot be WRITTEN inside it. Two sufficient shapes:
    ///   • it is OUT of the nursery — tenuring happens only at a collection, which ends
    ///     the window via the epoch stamp, so such an object necessarily pre-dates the
    ///     window and the window's effect proof forbids writing it; or
    ///   • its class is a VALUE RECORD (every non-static field `final` and primitive),
    ///     which the JVMS final-putfield rule freezes after its own `<init>`.
    /// `null` is trivially stable. Anything else refuses (the call runs, uncached).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn pure_memo_cell_stable(&mut self, cell: i64) -> bool {
        if cell == 0 {
            return true; // null carries no state
        }
        let raw = cell as u64;
        if raw >> 40 == 0 {
            let p = cell as *mut u8;
            let Some(ap) = crate::arena::ArenaPtr::new(p) else { return false };
            if !crate::nextgen::arena_contains(p) {
                return false; // not an object we can reason about
            }
            if !crate::nextgen::tlab_in_nursery(ap) {
                return true; // tenured/perm ⇒ predates the window ⇒ frozen by the proof
            }
            let Some(name) = crate::nextgen::class_name(crate::nextgen::arena_class_id(ap)) else {
                return false;
            };
            return self.pure_memo_value_record(&name);
        }
        if raw >> 40 == 1 {
            // Handle-heap object: no nursery watermark exists, so only the immutable
            // value-record shape qualifies.
            let h = (cell & (crate::value::REF_TAG - 1)) as u32;
            let Some(name) = self.heap.class_of(h).map(|c| c.to_string()) else { return false };
            return self.pure_memo_value_record(&name);
        }
        false
    }

    /// Does every non-static field of `name` (and of its superclasses) carry ACC_FINAL
    /// AND a primitive descriptor? Such an object's reachable graph is itself, and it
    /// is immutable after construction. Memoised per class.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn pure_memo_value_record(&mut self, name: &str) -> bool {
        if let Some(v) = crate::pure_memo::value_record_cached(name) {
            return v;
        }
        const ACC_STATIC: u16 = 0x0008;
        const ACC_FINAL: u16 = 0x0010;
        let mut ok = true;
        let chain = self.super_chain(name);
        for cn in chain.iter() {
            let Ok(c) = self.loader.get_or_load(cn) else {
                ok = false;
                break;
            };
            ok &= c.file.fields.iter().filter(|f| f.access_flags & ACC_STATIC == 0).all(|f| {
                f.access_flags & ACC_FINAL != 0
                    && matches!(
                        f.descriptor.as_bytes().first(),
                        Some(b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z')
                    )
            });
            if !ok {
                break;
            }
        }
        crate::pure_memo::value_record_put(name, ok);
        ok
    }
}
