//! Recursive admission proof for ordered shared-field transactions.

use super::lane_proof::note_write_set_open;
use crate::*;

impl Vm {
    /// FIELD-TXN DEVIRTUALISED proof: can this method's whole call-subtree run inside a
    /// WRITE-ONLY field transaction (writes to SHARED fields are journalled + replayed at
    /// the scope join, so a shared `putfield` is admissible), using the CONCRETE receiver
    /// type known at fork setup for devirtualisation. Same recursive + virtual-dispatch
    /// closure shape as `is_offloadable_devirt`, but each bytecode method is analysed by
    /// `method_field_txn_safe_given` instead of the escape analysis. Txn mode is
    /// FAIL-CLOSED on natively-served targets: unlike normal offload (which admits a
    /// `Pure, may_callback: false` native contract), a txn callee with NO bytecode has no
    /// journalled-write model — reject it and reject `<init>` bodies of any callee. The
    /// txn bit is a distinct `offload_cache` key slot (never collides with plain or
    /// relaxed verdicts); epoch invalidation is shared with the normal caches.
    #[allow(dead_code)]
    pub(crate) fn is_field_txn_safe_devirt(
        &mut self,
        concrete_recv: &str,
        name: &str,
        desc: &str,
    ) -> bool {
        self.offload_epoch_sync();
        let mut in_progress = std::collections::HashSet::new();
        self.is_field_txn_safe_rec(concrete_recv, name, desc, 0, &mut in_progress)
    }

    /// Recursive walker behind `is_field_txn_safe_devirt`. Same closure/cycle/cache
    /// protocol as `is_offloadable_rec` with the txn-specific callee contract folded in:
    /// EVERY invoke (including a fresh-receiver `new C` constructor site, where the
    /// analyzer delivers `q.exact == true` with the ctor as `name`) reaches `recurse`, so
    /// a native or no-bytecode target is rejected HERE — no Pure-native-contract
    /// admission in txn mode. Txn entries share the epoch guard but carry their own
    /// cache key, so normal `is_offloadable_rec` behavior is untouched.
    #[allow(dead_code)]
    pub(crate) fn is_field_txn_safe_rec(
        &mut self,
        class: &str,
        name: &str,
        desc: &str,
        depth: u32,
        in_progress: &mut std::collections::HashSet<usize>,
    ) -> bool {
        const TXN: bool = true;
        if name == "<init>" {
            return false;
        }
        let dry_run = crate::ClassLoader::dry_run_loads_active();
        if depth > 64 {
            if dry_run {
                note_write_set_open();
            }
            return false;
        }
        let decl = match self.resolve_method(class, name, desc) {
            Ok(Some(decl)) => decl,
            // FAIL-CLOSED: txn mode admits NO native target (a native write cannot be
            // journalled) — not even a Pure, non-callback contract.
            _ => return false,
        };
        let owner = decl.file.this_class.to_string();
        let Some(owner_class) = self.loader.cached(&owner) else {
            return false;
        };
        // A worker may not trigger initialization as a side effect of a call or
        // class-sensitive opcode. Every transitive bytecode owner must already
        // be fully initialized before the write transaction can be admitted.
        if !owner_class
            .clinit_done
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return false;
        }
        let Some(method) = decl.file.method(name, desc) else {
            return false;
        };
        let Some(code) = method.code.as_ref() else {
            return false;
        };
        // `new` can initialize its class even when a malformed/dead allocation
        // never reaches an invokespecial. Require that side effect to be settled
        // before the worker is admitted.
        let bytecode = &code.bytecode;
        let mut pc = 0;
        while pc < bytecode.len() {
            if bytecode[pc] == 0xbb {
                let cp = u16::from_be_bytes([
                    *bytecode.get(pc + 1).unwrap_or(&0),
                    *bytecode.get(pc + 2).unwrap_or(&0),
                ]);
                let Some(new_owner) = decl.file.pool.class_name(cp) else {
                    return false;
                };
                let Some(new_class) = self.loader.cached(new_owner) else {
                    return false;
                };
                if !new_class
                    .clinit_done
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    return false;
                }
            }
            let Some(len) = crate::jit::insn_len_at(bytecode, pc) else {
                return false;
            };
            let Some(next_pc) = pc.checked_add(len).filter(|end| *end <= bytecode.len()) else {
                return false;
            };
            pc = next_pc;
        }
        let key = (code as *const _ as usize, false, TXN, false);
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
                    // Txn: the recv_private native-admission shortcut is OFF; the exact
                    // target must itself pass the txn proof (bytecode or nothing).
                    self.is_field_txn_safe_rec(q.owner, q.name, q.desc, depth + 1, in_progress)
                } else {
                    match self.virtual_dispatch_closure(q.owner, q.name, q.desc) {
                        Some(targets) => targets.iter().all(|target| {
                            self.is_field_txn_safe_rec(
                                target,
                                q.name,
                                q.desc,
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
                            if is_loaded_iface {
                                let targets =
                                    self.interface_dispatch_closure(q.owner, q.name, q.desc);
                                !targets.is_empty()
                                    && targets.iter().all(|target| {
                                        self.is_field_txn_safe_rec(
                                            target,
                                            q.name,
                                            q.desc,
                                            depth + 1,
                                            in_progress,
                                        )
                                    })
                            } else {
                                // No provable closure ⇒ some runtime receiver could hit a
                                // natively-served impl ⇒ fail closed (no contract fallback:
                                // even a Pure native is a reject in txn mode).
                                false
                            }
                        }
                    }
                }
            };
            let bsm = Some(decl.file.bootstrap_methods.as_slice());
            crate::offload::method_field_txn_safe_given(code, pool, bsm, recurse)
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

#[cfg(test)]
mod tests {
    use super::*;
    use rava_classfile::{ClassFile, Code, Constant, ConstantPool, MemberInfo};

    #[test]
    fn write_transaction_requires_initialized_owners_and_new_targets() {
        let mut vm = Vm::new(crate::class_loader::ClassLoader::new());
        let pool = ConstantPool::from_entries(vec![
            Constant::Unusable,
            Constant::Class { name_index: 2 },
            Constant::Utf8("TxnInitTarget".into()),
        ]);
        let root = vm.loader.define_synthetic(ClassFile::synthesize(
            pool,
            0x0021,
            "TxnInitRoot".into(),
            None,
            Vec::new(),
            vec![MemberInfo {
                access_flags: 0x0009,
                name: "safe".into(),
                descriptor: "()V".into(),
                code: Some(Code {
                    max_stack: 1,
                    max_locals: 0,
                    bytecode: vec![0xbb, 0x00, 0x01, 0x57, 0xb1],
                    ..Default::default()
                }),
            }],
        ));
        let target = vm.loader.define_synthetic(ClassFile::synthesize(
            ConstantPool::from_entries(vec![Constant::Unusable]),
            0x0021,
            "TxnInitTarget".into(),
            None,
            Vec::new(),
            Vec::new(),
        ));

        assert!(!vm.is_field_txn_safe_devirt("TxnInitRoot", "safe", "()V"));
        vm.ensure_init(&root).expect("initialize the root owner");
        assert!(!vm.is_field_txn_safe_devirt("TxnInitRoot", "safe", "()V"));
        vm.ensure_init(&target)
            .expect("initialize the allocated type");
        assert!(vm.is_field_txn_safe_devirt("TxnInitRoot", "safe", "()V"));
    }
}
