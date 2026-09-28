//! Metadata-resolved prepared calls without a synthetic interpreter frame.
use crate::*;

/// Linux tracer bullet for cached, monomorphic `invokestatic` targets.
pub(crate) fn linux_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RAVA_AOT_DIRECT").as_deref() == Ok("1"))
}

#[cfg(not(target_os = "linux"))]
fn direct_resolution_cache_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    jit::env_gate("RAVA_AOT_RESOLVED_CACHE", &ON)
}

#[cfg(target_os = "linux")]
fn direct_resolution_cache_enabled() -> bool {
    linux_enabled() || {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        jit::env_gate("RAVA_AOT_RESOLVED_CACHE", &ON)
    }
}

impl Vm {
    pub(crate) fn aot_direct_call(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        kind: u8,
        raw: &[i64],
    ) -> Option<Result<i64, VmError>> {
        if !matches!(kind, 0 | 1 | 3)
            || caller_ptr.is_null()
            || self.jit_verify
            || self.trampoline
            || crate::parallel::gilfree()
            || !crate::aot_clif::prepared::enabled()
        {
            return None;
        }
        // The callback ABI keeps the caller class alive for the entire invocation.
        let caller = unsafe { &*caller_ptr };
        let member = caller.file.pool.member_ref(cp)?;
        let is_static = kind == 0;
        if member.name.starts_with('<')
            || count_args(member.descriptor) + usize::from(!is_static) != raw.len()
        {
            return None;
        }
        // The prepared raw-cell ABI cannot expose an arena reference array as the legacy
        // handle that AOT callbacks and callee code expect.  Decline only the sites whose
        // immutable JVM shape says that the receiver or an argument is an array; the normal
        // `jit_call_body` marshal then materializes those cells through `ArrayRef::materialize`.
        // Keep this structural: no receiver class/name, hotness, or prior-run observation.
        let receiver_array =
            !is_static && member.owner.as_bytes().first() == Some(&b'[');
        let argument_array = crate::args_of(member.descriptor).as_bytes().contains(&b'[');
        if receiver_array || argument_array {
            return None;
        }
        // Linking constants can load classes and collect. Root raw arguments before
        // the shared AOT entry performs any linking, just as jit_direct_call does.
        frames::cells_push((raw.as_ptr(), raw.len()));
        let result = (|| {
            let resolve_time = crate::aot_clif::diagnostics::resolve().start();
            let memo_key = if direct_resolution_cache_enabled() {
                let receiver_id = if is_static {
                    Some(0)
                } else {
                    match jit::unbox_ref(self, *raw.first()?, "") {
                        Value::Obj(object) => Some(crate::nextgen::arena_class_id(object)),
                        _ => None,
                    }
                };
                receiver_id.map(|id| (caller_ptr as usize, cp, kind, id))
            } else {
                None
            };
            let cached =
                memo_key.and_then(|key| self.aot_direct_targets.get(self.loader.epoch(), &key));
            let target = if let Some(target) = cached {
                target
            } else {
                // An interface can remain unloaded after its implementing object is
                // constructed. Resolve metadata without running its initializer.
                let owner = self.loader.get_or_load(member.owner).ok()?;
                if member.owner != caller.file.this_class && owner.file.access_flags & 1 == 0 {
                    return None;
                }
                if !is_static {
                    let declared = owner.file.method(member.name, member.descriptor)?;
                    if declared.access_flags & 1 == 0 || declared.access_flags & 8 != 0 {
                        return None;
                    }
                }
                let class = if is_static {
                    owner
                } else {
                    let receiver = jit::unbox_ref(self, raw[0], "");
                    let class = match receiver {
                        Value::Obj(object) => {
                            let name =
                                crate::nextgen::class_name(crate::nextgen::arena_class_id(object))?;
                            self.loader.get_loaded(&name)?
                        }
                        Value::Ref(handle) => {
                            self.loader.get_loaded(self.heap.class_of(handle)?)?
                        }
                        _ => return None,
                    };
                    if class.file.this_class != member.owner
                        && !self
                            .loader
                            .type_closure(&class.file.this_class)
                            .contains(member.owner)
                    {
                        return None;
                    }
                    class
                };
                // Declared targets only: inherited/default-method resolution stays on
                // the general path. Exact receiver selection preserves overrides.
                let method_index = class.file.methods.iter().position(|method| {
                    method.name == member.name && method.descriptor == member.descriptor
                })?;
                let method = &class.file.methods[method_index];
                if (method.access_flags & 0x8 != 0) != is_static
                    || method.access_flags & 0x520 != 0
                    || ((!is_static || member.owner != caller.file.this_class)
                        && method.access_flags & 1 == 0)
                {
                    return None;
                }
                let target = super::aot_direct_cache::Target {
                    class,
                    method: method_index,
                };
                if let Some(key) = memo_key {
                    self.aot_direct_targets
                        .insert(self.loader.epoch(), key, target.clone());
                    static RESOLVE_TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                    if jit::env_gate("RAVA_AOT_TRACE_DIRECT", &RESOLVE_TRACE) {
                        eprintln!(
                            "[AOT-RESOLVE] {}#{}{}",
                            target.class.file.this_class, member.name, member.descriptor
                        );
                    }
                }
                target
            };
            let class = target.class;
            let method = &class.file.methods[target.method];
            let code = method.code.as_ref()?;
            #[cfg(target_os = "linux")]
            if !matches!(
                self.aot_full_cache.get(&(code as *const _ as usize)),
                Some(Some(_))
            ) {
                return None;
            }
            drop(resolve_time);
            let result = self.aot_run_input(
                code,
                member.name,
                member.descriptor,
                is_static,
                &class,
                super::aot_arguments::Arguments::Raw(raw),
            )?;
            static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if jit::env_gate("RAVA_AOT_TRACE_DIRECT", &TRACE) {
                eprintln!(
                    "[AOT-DIRECT] {}#{}{}",
                    class.file.this_class, member.name, member.descriptor
                );
            }
            Some(result)
        })();
        frames::cells_pop();
        let result = result?;
        Some(result.map(|value| match value {
            None => 0,
            Some(value) => {
                let ret = member
                    .descriptor
                    .rsplit_once(')')
                    .map(|(_, ret)| ret)
                    .unwrap_or("");
                if ret.starts_with('L') || ret.starts_with('[') {
                    jit::box_ref(self, &value)
                } else {
                    jit::pack_local(&value)
                }
            }
        }))
    }
}
