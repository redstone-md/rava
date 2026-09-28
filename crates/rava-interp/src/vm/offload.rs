//! Whether a call may leave the GIL, and the epoch that keeps workers coherent.

use super::lane_proof::{
    note_dispatch_open, note_observed_code, note_write_set_open, with_lane_proof_facts,
    LaneProofCensusReport,
};
use crate::*;

impl Vm {
    /// GAP-S09 debug mutation guard: shared `Vm` caches are FROZEN inside a gil-free
    /// region — no worker (nor any code running with the gil-free flag) may mutate
    /// them. Release builds rely on the structural guards (S07 loader/dispatch
    /// aborts); debug builds fail fast at the exact violating site.
    /// `pub(crate)`: stage 2 extends the guard to frozen-table mutation sites in
    /// `opcodes.rs`/`heap.rs` (see docs/gil-free-vm-layout.md for the field census).
    ///
    /// `RAVA_FREEZE_TALLY=1` additionally COUNTS, in release, which of these sites a
    /// gil-free worker actually reaches. That is the measurement the lane's ceiling
    /// turns on: `worker_locked_exec` holds one global lock across a callee's whole
    /// EXECUTION because the general path *might* mutate shared state — but most of
    /// these twenty sites are cache FILLS, where skipping the insert is semantically
    /// neutral (you simply do not memoise). If the ring only ever reaches fillable
    /// sites, the lock can be replaced by "run frozen", and the 82%-of-region hold
    /// disappears. If it reaches a non-fillable one (`write_static_cell`,
    /// `indy_singletons`), that site is the real blocker and is named here.
    #[inline]
    pub(crate) fn debug_assert_not_gilfree(what: &str) {
        // THE ENVELOPE IS AN EXEMPTION, and it always was — this guard just did not say so.
        //
        // `jit::vm_ref` already admits a full `&mut Vm` on a gil-free worker when
        // `worker_locked_exec_enabled() && resolve_lock_held_by_me()`, on the grounds that main is
        // parked at the scope join and every sibling blocks on that same re-entrant lock, so the
        // holder has exactly the exclusivity `&mut` denotes. A guard that forbids ALL gil-free
        // mutation therefore contradicts the engine's own admission rule; it went unnoticed only
        // because it is a `debug_assert` and the lane was opt-in, so the situation never arose in
        // a build that checks it.
        //
        // Outside the envelope nothing is relaxed: an unlocked gil-free mutation still fires.
        #[cfg(feature = "next-gen-gc")]
        let inside_envelope =
            crate::worker_locked_exec_enabled() && crate::jit::resolve_lock_held_by_me();
        #[cfg(not(feature = "next-gen-gc"))]
        let inside_envelope = false;
        debug_assert!(
            !crate::parallel::gilfree() || inside_envelope,
            "GAP-S09: a gil-free worker mutated frozen shared VM state OUTSIDE the locked \
             envelope: {what}"
        );
        // The tally still counts every gil-free touch, envelope or not — "how often does a worker
        // reach shared state at all" is the measurement, and exempting the envelope from the
        // ASSERT must not quietly exempt it from the COUNT.
        if crate::freeze_tally::enabled() && crate::parallel::gilfree() {
            crate::freeze_tally::note(what);
        }
    }

    /// GAP-S06/S02: drop every hierarchy-dependent memoised verdict when the loader
    /// epoch moved (a newly loaded class can add an override that invalidates any
    /// closure-ranging admission). Cheap when unchanged (one u64 compare).
    pub(crate) fn offload_epoch_sync(&mut self) {
        if crate::ClassLoader::dry_run_loads_active() {
            return;
        }
        let e = self.loader.epoch();
        if e != self.offload_epoch {
            crate::offload_probe::epoch_clear(self.offload_cache.len());
            self.offload_cache.clear();
            self.dispatch_closure_cache.clear();
            self.unloadable_owner_closed_cache.clear();
            self.iface_closure_cache.clear();
            self.offload_epoch = e;
        }
    }

    /// PARALLEL-WORLDGEN #21: can this method's WHOLE call-subtree run GIL-free on a
    /// worker — i.e. no side effect another thread could observe (see `offload`)?
    /// Transitive over invokes, memoised by Code pointer, cycle-guarded. Conservative — a
    /// contract-less native/unresolved callee, or anything the analyzer can't prove safe,
    /// is `false`. GAP-S06: virtual/interface callees on receivers without an exact type
    /// are admitted only through `virtual_dispatch_closure` (all targets must pass).
    #[allow(dead_code)]
    pub(crate) fn is_offloadable(
        &mut self,
        class: &str,
        name: &str,
        desc: &str,
        relaxed: bool,
    ) -> bool {
        self.offload_epoch_sync();
        let mut in_progress = std::collections::HashSet::new();
        self.is_offloadable_rec(class, name, desc, relaxed, false, 0, &mut in_progress)
    }

    /// F2/F3 DEVIRTUALISED offloadability: check the per-element method against the CONCRETE
    /// receiver type known at fork setup (the loop's element class), not the call's abstract/
    /// interface declared owner. `resolve_method` already devirtualises virtual/interface
    /// dispatch from a concrete start class (walking supers + default methods), so starting the
    /// transitive escape analysis at `concrete_recv` resolves the abstract per-element call
    /// (`DiscreteVoxelShape.isFull`, `IndexMerger.size`) to its concrete impl. The fork gate
    /// calls this; it is sound because the concrete element type IS known at runtime fork setup.
    /// `receiver_private` = the ELEMENT-OWNERSHIP flag: the per-element method's `this` is the
    /// distinct per-iteration element, so its own field writes are disjoint (see
    /// `offload::method_offloadable_forked`). Applies to the ROOT method only; recursive callees
    /// analyse with `this` shared (transitive element-ownership is a later increment).
    #[allow(dead_code)]
    pub(crate) fn is_offloadable_devirt(
        &mut self,
        concrete_recv: &str,
        name: &str,
        desc: &str,
        relaxed: bool,
        receiver_private: bool,
    ) -> bool {
        self.offload_epoch_sync();
        let mut in_progress = std::collections::HashSet::new();
        self.is_offloadable_rec(
            concrete_recv,
            name,
            desc,
            relaxed,
            receiver_private,
            0,
            &mut in_progress,
        )
    }

    /// Diagnostic (RAVA_OFFLOAD_PROBE): the first reason this method's TOP-LEVEL body
    /// can't be offloaded — names the blocking opcode or the non-offloadable callee.
    #[allow(dead_code)]
    pub(crate) fn offload_reason(
        &mut self,
        class: &str,
        name: &str,
        desc: &str,
        relaxed: bool,
    ) -> Option<String> {
        let decl = match self.resolve_method(class, name, desc) {
            Ok(Some(d)) => d,
            _ => return Some("native/unresolved".into()),
        };
        let Some(code) = decl.file.method(name, desc).and_then(|m| m.code.as_ref()) else {
            return Some("no-code".into());
        };
        let mut in_progress = std::collections::HashSet::new();
        let pool = &decl.file.pool;
        let bsm = Some(decl.file.bootstrap_methods.as_slice());
        crate::offload::method_offloadable_reason(
            code,
            pool,
            bsm,
            relaxed,
            name == "<init>",
            &mut |q: crate::offload::CalleeQuery| {
                if q.exact {
                    self.is_offloadable_rec(
                        q.owner,
                        q.name,
                        q.desc,
                        relaxed,
                        false,
                        1,
                        &mut in_progress,
                    ) || (q.recv_private
                        && self.private_throwable_ctor_admissible(q.owner, q.name, q.desc))
                } else {
                    match self.virtual_dispatch_closure(q.owner, q.name, q.desc) {
                        Some(ts) => ts.iter().all(|t| {
                            self.is_offloadable_rec(
                                t,
                                q.name,
                                q.desc,
                                relaxed,
                                false,
                                1,
                                &mut in_progress,
                            )
                        }),
                        None => self.unloadable_owner_contract_fallback(q.owner, q.name, q.desc),
                    }
                }
            },
        )
        .map(|r| r.to_string())
    }

    /// Element-ownership (receiver-private) variant of `offload_reason`: the first blocker of the
    /// per-element method analysed with `this` writable + devirtualised against `concrete_recv`.
    #[allow(dead_code)]
    pub(crate) fn offload_reason_devirt(
        &mut self,
        concrete_recv: &str,
        name: &str,
        desc: &str,
        relaxed: bool,
    ) -> Option<String> {
        let decl = match self.resolve_method(concrete_recv, name, desc) {
            Ok(Some(d)) => d,
            _ => {
                note_write_set_open();
                return Some("native/unresolved".into());
            }
        };
        let Some(code) = decl.file.method(name, desc).and_then(|m| m.code.as_ref()) else {
            note_write_set_open();
            return Some("no-code".into());
        };
        // Include the root body too: recursive callees are recorded by
        // `is_offloadable_rec`, but this diagnostic entry point analyzes its root directly.
        note_observed_code(code as *const _ as usize);
        let mut in_progress = std::collections::HashSet::new();
        let pool = &decl.file.pool;
        let bsm = Some(decl.file.bootstrap_methods.as_slice());
        let reason = crate::offload::method_offloadable_reason_forked(
            code,
            pool,
            bsm,
            relaxed,
            Some(concrete_recv),
            &mut |q: crate::offload::CalleeQuery| {
                if q.exact {
                    self.is_offloadable_rec(
                        q.owner,
                        q.name,
                        q.desc,
                        relaxed,
                        false,
                        1,
                        &mut in_progress,
                    ) || (q.recv_private
                        && self.private_throwable_ctor_admissible(q.owner, q.name, q.desc))
                } else {
                    match self.virtual_dispatch_closure(q.owner, q.name, q.desc) {
                        Some(ts) => ts.iter().all(|t| {
                            self.is_offloadable_rec(
                                t,
                                q.name,
                                q.desc,
                                relaxed,
                                false,
                                1,
                                &mut in_progress,
                            )
                        }),
                        None => {
                            let admitted =
                                self.unloadable_owner_contract_fallback(q.owner, q.name, q.desc);
                            if !admitted {
                                note_dispatch_open();
                            }
                            admitted
                        }
                    }
                }
            },
        );
        // A rejection of the root body itself is also an unproved write/effect set. The recursive
        // walker marks this for callees; mirror it here so a root shared-write cannot be reported
        // as a closed set merely because its body was the analysis starting point.
        if crate::ClassLoader::dry_run_loads_active() && reason.is_some() {
            note_write_set_open();
        }
        reason.map(|r| r.to_string())
    }

    /// Run the same structural proof with all class loads staged outside the live loader state.
    pub(crate) fn offload_census_devirt(
        &mut self,
        concrete_recv: &str,
        name: &str,
        desc: &str,
    ) -> LaneProofCensusReport {
        crate::ClassLoader::with_dry_run_loads(|| {
            let staged_before = self.loader.dry_run_staged_count();
            let (reason, facts) = with_lane_proof_facts(|| {
                self.offload_reason_devirt(concrete_recv, name, desc, false)
            });
            let dry_loaded = self
                .loader
                .dry_run_staged_count()
                .saturating_sub(staged_before);
            let dry_names = self.loader.dry_run_staged_names();
            let store_join = crate::jit::emit::store_census::join(&facts.observed_codes);
            LaneProofCensusReport {
                dispatch_closed: !facts.dispatch_open,
                class_load_closed: dry_loaded == 0,
                write_set_closed: !facts.write_set_open,
                dry_loaded,
                dry_names,
                reason,
                store_join,
            }
        })
    }

    #[allow(dead_code)]
    pub(crate) fn is_offloadable_rec(
        &mut self,
        class: &str,
        name: &str,
        desc: &str,
        relaxed: bool,
        receiver_private: bool,
        depth: u32,
        in_progress: &mut std::collections::HashSet<usize>,
    ) -> bool {
        let dry_run = crate::ClassLoader::dry_run_loads_active();
        if depth > 64 {
            if dry_run {
                note_write_set_open();
            }
            return false;
        }
        let decl = match self.resolve_method(class, name, desc) {
            Ok(Some(decl)) => decl,
            _ => {
                let flags = self
                    .loader
                    .get_or_load(class)
                    .ok()
                    .map(|c| c.file.access_flags);
                let admitted = matches!(
                    crate::native_contract::contract_for(class, name, desc, flags),
                    Some(crate::native_contract::NativeContract {
                        effect: crate::native_contract::NativeEffect::Pure,
                        may_callback: false,
                        ..
                    })
                );
                if dry_run && !admitted {
                    note_write_set_open();
                }
                return admitted;
            }
        };
        let Some(method) = decl.file.method(name, desc) else {
            if dry_run {
                note_write_set_open();
            }
            return false;
        };
        let Some(code) = method.code.as_ref() else {
            if dry_run {
                note_write_set_open();
            }
            return false;
        };
        // LANE STORE-FORM CENSUS join (diagnostic only): every Code identity the proof
        // visits, root included, even a memoised one (a memo hit is still an observed
        // body — its absence from the census is the unknown the report must show).
        note_observed_code(code as *const _ as usize);
        let key = (code as *const _ as usize, relaxed, false, receiver_private);
        if !dry_run {
            if let Some(&cached) = self.offload_cache.get(&key) {
                crate::offload_probe::hit();
                return cached;
            }
        }
        if !in_progress.insert(key.0) {
            return true;
        }
        crate::offload_probe::analyzed();
        let entry_epoch = self.loader.epoch();
        let result = {
            let pool = &decl.file.pool;
            let recurse = &mut |q: crate::offload::CalleeQuery| {
                if q.exact {
                    self.is_offloadable_rec(
                        q.owner,
                        q.name,
                        q.desc,
                        relaxed,
                        false,
                        depth + 1,
                        in_progress,
                    ) || (q.recv_private
                        && self.private_throwable_ctor_admissible(q.owner, q.name, q.desc))
                } else {
                    match self.virtual_dispatch_closure(q.owner, q.name, q.desc) {
                        Some(targets) => targets.iter().all(|target| {
                            self.is_offloadable_rec(
                                target,
                                q.name,
                                q.desc,
                                relaxed,
                                false,
                                depth + 1,
                                in_progress,
                            )
                        }),
                        None => {
                            const ACC_INTERFACE: u16 = 0x0200;
                            let is_loaded_iface = self
                                .loader
                                .cached(q.owner)
                                .is_some_and(|c| c.file.access_flags & ACC_INTERFACE != 0);
                            let admitted = if is_loaded_iface {
                                let targets =
                                    self.interface_dispatch_closure(q.owner, q.name, q.desc);
                                !targets.is_empty()
                                    && targets.iter().all(|target| {
                                        self.is_offloadable_rec(
                                            target,
                                            q.name,
                                            q.desc,
                                            relaxed,
                                            false,
                                            depth + 1,
                                            in_progress,
                                        )
                                    })
                            } else {
                                self.unloadable_owner_contract_fallback(q.owner, q.name, q.desc)
                            };
                            if dry_run && !admitted {
                                note_dispatch_open();
                            }
                            admitted
                        }
                    }
                }
            };
            let bsm = Some(decl.file.bootstrap_methods.as_slice());
            if dry_run {
                let reason = if receiver_private {
                    crate::offload::method_offloadable_reason_forked(
                        code,
                        pool,
                        bsm,
                        relaxed,
                        Some(class),
                        recurse,
                    )
                } else {
                    crate::offload::method_offloadable_reason(
                        code,
                        pool,
                        bsm,
                        relaxed,
                        name == "<init>",
                        recurse,
                    )
                };
                if reason.is_some() {
                    note_write_set_open();
                }
                reason.is_none()
            } else if receiver_private {
                crate::offload::method_offloadable_forked(
                    code,
                    pool,
                    bsm,
                    relaxed,
                    Some(class),
                    recurse,
                )
            } else {
                crate::offload::method_offloadable_given(
                    code,
                    pool,
                    bsm,
                    relaxed,
                    name == "<init>",
                    recurse,
                )
            }
        };
        in_progress.remove(&key.0);
        if !dry_run {
            if self.loader.epoch() == entry_epoch {
                Self::debug_assert_not_gilfree("offload_cache");
                self.offload_cache.insert(key, result);
                crate::offload_probe::stored();
            } else {
                crate::offload_probe::unstable();
            }
        }
        result
    }
}
