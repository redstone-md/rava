//! Narrow dispatch-miss diagnostics for voxel BooleanOp sites and DataResult$Success::apply.
//!
//! `rava_dispatch_miss` normally receives only `(caller class, cp)`. An opt-in site token carries
//! the compiling method and bytecode offset; DataResult samples also record arena LambdaSite IDs.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Mutex;

use crate::class_loader::LoadedClass;
use crate::Vm;
use rava_classfile::Code;

struct Site {
    scope: Scope,
    caller_class: String,
    caller_method: String,
    caller_desc: String,
    bci: usize,
    cp: u16,
    target_owner: String,
    target_name: String,
    target_desc: String,
    stats: Mutex<SiteStats>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scope {
    Voxel,
    DataResult,
}

impl Scope {
    fn label(self) -> &'static str {
        match self {
            Self::Voxel => "voxel",
            Self::DataResult => "dataresult",
        }
    }
}

#[derive(Default)]
struct SiteStats {
    by_class: HashMap<ReceiverKey, u64>,
    by_cache: HashMap<(i64, ReceiverKind, i64), u64>,
    by_lambda_cache: HashMap<(i64, ReceiverKey), u64>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ReceiverKey {
    kind: ReceiverKind,
    value: i64,
    lambda_site: Option<u32>,
}


#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ReceiverKind {
    Arena,
    ArenaLambda,
    HeapRef,
    Other,
}

impl ReceiverKind {
    fn label(self) -> &'static str {
        match self {
            Self::Arena => "arena",
            Self::ArenaLambda => "arena-lambda",
            Self::HeapRef => "heap-ref",
            Self::Other => "other",
        }
    }
}


#[derive(Clone, Copy)]
struct ActiveMiss {
    site: usize,
    seen_site: u64,
    seen_cache: u64,
    seen_lambda_cache: Option<u64>,
    report: bool,
}

thread_local! {
    static ACTIVE: RefCell<Vec<ActiveMiss>> = const { RefCell::new(Vec::new()) };
}

static SITES: Mutex<Vec<usize>> = Mutex::new(Vec::new());

pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_DIAG_DISPATCH").is_some()
            || std::env::var_os("RAVA_DIAG_VOXEL_DISPATCH").is_some()
    })
}

/// Register only the requested class/method references. The leaked descriptor is embedded in the
/// miss arm, so it remains valid for the lifetime of generated code, including worker copies.
pub(crate) fn register_site(class_ptr: i64, code_key: usize, bci: usize, cp: u16) -> i64 {
    if !enabled() || class_ptr == 0 || code_key == 0 {
        return 0;
    }

    let caller = unsafe { &*(class_ptr as *const LoadedClass) };
    let caller_name = caller.file.this_class.as_str();
    let Some(reference) = caller.file.pool.member_ref(cp) else {
        return 0;
    };
    let scope = if (caller_name == "net/minecraft/world/phys/shapes/Shapes"
        || caller_name == "net/minecraft/world/phys/shapes/BitSetDiscreteVoxelShape")
        && reference.owner.ends_with("/BooleanOp")
        && reference.name == "apply"
        && reference.descriptor == "(ZZ)Z"
    {
        Scope::Voxel
    } else if caller_name == "com/mojang/serialization/DataResult$Success"
        && reference.owner == "java/util/function/Function"
        && reference.name == "apply"
        && reference.descriptor == "(Ljava/lang/Object;)Ljava/lang/Object;"
    {
        Scope::DataResult
    } else {
        return 0;
    };

    let code = unsafe { &*(code_key as *const Code) };
    if bci + 2 >= code.bytecode.len()
        || !matches!(code.bytecode[bci], 0xb6 | 0xb9)
        || u16::from_be_bytes([code.bytecode[bci + 1], code.bytecode[bci + 2]]) != cp
    {
        return 0;
    }

    let Some(method) = caller.file.methods.iter().find(|method| {
        method
            .code
            .as_ref()
            .is_some_and(|candidate| std::ptr::eq(candidate, code))
    }) else {
        return 0;
    };

    let site = Box::new(Site {
        scope,
        caller_class: caller_name.to_owned(),
        caller_method: method.name.clone(),
        caller_desc: method.descriptor.clone(),
        bci,
        cp,
        target_owner: reference.owner.to_owned(),
        target_name: reference.name.to_owned(),
        target_desc: reference.descriptor.to_owned(),
        stats: Mutex::new(SiteStats::default()),
    });
    let ptr = Box::into_raw(site) as usize;
    SITES.lock().unwrap_or_else(|e| e.into_inner()).push(ptr);
    ptr as i64
}

/// Record one callback before resolution, keyed both by the logical BCI and the physical
/// per-thread cache. A repeated class on a different worker cache is therefore not mislabeled as
/// a broken same-cache hit.
pub(crate) struct MissGuard {
    site: &'static Site,
    site_ptr: usize,
    cache_ptr: i64,
    receiver: ReceiverKey,
    receiver_name: String,
    class_id: Option<u32>,
    slot_before: String,
    active: ActiveMiss,
}

impl MissGuard {
    pub(crate) fn begin(site_ptr: i64, cache_ptr: i64, recv: i64, vm: &Vm) -> Option<Self> {
        if !enabled() || site_ptr == 0 {
            return None;
        }
        let site_ptr = site_ptr as usize;
        let site = unsafe { &*(site_ptr as *const Site) };
        let (receiver, receiver_name, class_id) =
            receiver(vm, recv, site.scope == Scope::DataResult);
        let slot_before = cache_entry(cache_ptr, receiver.value, ways())
            .map_or_else(|| "absent".to_owned(), cache_label);
        let (seen_site, seen_cache, seen_lambda_cache) = {
            let mut stats = site.stats.lock().unwrap_or_else(|e| e.into_inner());
            let seen_site = {
                let count = stats.by_class.entry(receiver).or_default();
                *count += 1;
                *count
            };
            let seen_cache = {
                let count = stats
                    .by_cache
                    .entry((cache_ptr, receiver.kind, receiver.value))
                    .or_default();
                *count += 1;
                *count
            };
            let seen_lambda_cache = receiver.lambda_site.map(|_| {
                let count = stats
                    .by_lambda_cache
                    .entry((cache_ptr, receiver))
                    .or_default();
                *count += 1;
                *count
            });
            (seen_site, seen_cache, seen_lambda_cache)
        };
        let active = ActiveMiss {
            site: site_ptr,
            seen_site,
            seen_cache,
            seen_lambda_cache,
            report: should_report(seen_site),
        };
        ACTIVE.with(|stack| stack.borrow_mut().push(active));
        Some(Self {
            site,
            site_ptr,
            cache_ptr,
            receiver,
            receiver_name,
            class_id,
            slot_before,
            active,
        })
    }
}

impl Drop for MissGuard {
    fn drop(&mut self) {
        if self.active.report {
            let slot_after = cache_entry(self.cache_ptr, self.receiver.value, ways())
                .map_or_else(|| "absent".to_owned(), cache_label);
            eprintln!(
                "[DISPATCH-DIAG] target={} caller={}.{}{} bci={} cp={} target_method={}.{}{} receiver={} class={} class_id={:?} lambda_site={:?} cache={:#x} seen_site={} seen_cache={} seen_lambda_cache={:?} before={} after={}",
                self.site.scope.label(),
                self.site.caller_class,
                self.site.caller_method,
                self.site.caller_desc,
                self.site.bci,
                self.site.cp,
                self.site.target_owner,
                self.site.target_name,
                self.site.target_desc,
                self.receiver.kind.label(),
                self.receiver_name,
                self.class_id,
                self.receiver.lambda_site,
                self.cache_ptr,
                self.active.seen_site,
                self.active.seen_cache,
                self.active.seen_lambda_cache,
                self.slot_before,
                slot_after,
            );
        }
        ACTIVE.with(|stack| {
            let popped = stack.borrow_mut().pop();
            debug_assert_eq!(popped.map(|context| context.site), Some(self.site_ptr));
        });
    }
}

/// Called at the same classifier decision that populates the PIC. A positive result encodes
/// leaf/deep in bit 0; negative results name the gate that prevented a fill.
pub(crate) fn note_resolution(
    receiver_class: &str,
    implementation_class: &str,
    method: &str,
    descriptor: &str,
    leaf: Option<bool>,
    fill: &str,
    reason: &str,
) {
    let Some(active) = active_context() else {
        return;
    };
    if !active.report {
        return;
    }
    let site = unsafe { &*(active.site as *const Site) };
    if method != site.target_name || descriptor != site.target_desc {
        return;
    }
    eprintln!(
        "[DISPATCH-RESOLVE] target={} caller={}.{}{} bci={} cp={} receiver_class={} impl={}.{}{} is_gc_leaf={} fill_result={} reason={} seen_site={}",
        site.scope.label(),
        site.caller_class,
        site.caller_method,
        site.caller_desc,
        site.bci,
        site.cp,
        receiver_class,
        implementation_class,
        method,
        descriptor,
        leaf.map_or_else(|| "not-tested".to_owned(), |v| v.to_string()),
        fill,
        reason,
        active.seen_site,
    );
}

fn active_context() -> Option<ActiveMiss> {
    ACTIVE.with(|stack| stack.borrow().last().copied())
}

fn should_report(seen: u64) -> bool {
    seen <= 4 || seen.is_power_of_two()
}

fn receiver(vm: &Vm, recv: i64, track_lambda_site: bool) -> (ReceiverKey, String, Option<u32>) {
    let raw = recv as u64;
    if recv != 0 && raw >> 40 == 0 {
        let class_id = unsafe { *(recv as *const u32) };
        let name = crate::nextgen::class_name(class_id)
            .map_or_else(|| "<unknown-arena-class>".to_owned(), |n| n.to_string());
        let class_kind = if name.starts_with("[arena-lambda:") {
            ReceiverKind::ArenaLambda
        } else {
            ReceiverKind::Arena
        };
        let lambda_site = (track_lambda_site && class_kind == ReceiverKind::ArenaLambda).then(|| {
            unsafe {
                *((recv as *const u8)
                    .add(crate::nextgen::HDR + crate::nextgen::AL_SITE_SLOT * 8)
                    as *const i64) as u32
            }
        });
        return (
            ReceiverKey {
                kind: class_kind,
                value: class_id as i64,
                lambda_site,
            },
            name,
            Some(class_id),
        );
    }
    if raw >> 40 == 1 {
        let handle = (recv & (crate::value::REF_TAG - 1)) as u32;
        let name = vm
            .heap
            .class_of(handle)
            .map(str::to_owned)
            .unwrap_or_else(|| "<stale-heap-ref>".to_owned());
        let key = vm.heap.object_class_word(handle).unwrap_or(0);
        return (
            ReceiverKey {
                kind: ReceiverKind::HeapRef,
                value: key,
                lambda_site: None,
            },
            name,
            None,
        );
    }
    (
        ReceiverKey {
            kind: ReceiverKind::Other,
            value: recv,
            lambda_site: None,
        },
        format!("tagged:{:#x}", raw),
        None,
    )
}

fn ways() -> usize {
    if crate::gil::is_worker() && crate::jit::ic_parallel_enabled() {
        crate::jit::IC_WORKER_WAYS
    } else {
        4
    }
}

fn cache_entry(cache_ptr: i64, key: i64, ways: usize) -> Option<i64> {
    if cache_ptr == 0 {
        return None;
    }
    let cache = cache_ptr as *const i64;
    unsafe {
        (0..ways).find_map(|slot| {
            let slot_key = *cache.add(slot * 2);
            let fill = *cache.add(slot * 2 + 1);
            (slot_key == key && fill != 0).then_some(fill)
        })
    }
}

fn fill_label(value: i64) -> &'static str {
    match value {
        -1 => "never-cacheable",
        0 => "empty",
        _ if value > 0 => crate::jit::pic_native::descriptor_kind(value as usize)
            .unwrap_or("positive-code"),
        _ => "other",
    }
}

fn cache_label(value: i64) -> String {
    format!("{}:{value:#x}", fill_label(value))
}

/// Print bounded per-site totals on normal process exit. Live-server runs also emit the first
/// few and power-of-two miss transitions directly from the slow path.
pub(crate) fn dump() {
    if !enabled() {
        return;
    }
    let sites = SITES.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for ptr in sites {
        let site = unsafe { &*(ptr as *const Site) };
        let stats = site.stats.lock().unwrap_or_else(|e| e.into_inner());
        if stats.by_class.is_empty() {
            continue;
        }
        eprintln!(
            "[DISPATCH-SUMMARY] target={} caller={}.{}{} bci={} cp={} target_method={}.{}{} receiver_variants={} callbacks={}",
            site.scope.label(),
            site.caller_class,
            site.caller_method,
            site.caller_desc,
            site.bci,
            site.cp,
            site.target_owner,
            site.target_name,
            site.target_desc,
            stats.by_class.len(),
            stats.by_class.values().sum::<u64>(),
        );
        for (receiver, count) in &stats.by_class {
            let (cache_ptrs, cache_callbacks) = stats
                .by_cache
                .iter()
                .filter(|((_, kind, value), _)| {
                    *kind == receiver.kind && *value == receiver.value
                })
                .fold((0usize, 0u64), |(pointers, callbacks), (_, n)| {
                    (pointers + 1, callbacks + n)
                });
            let lambda_cache_callbacks = stats
                .by_lambda_cache
                .iter()
                .filter(|((_, key), _)| key == receiver)
                .map(|(_, count)| count)
                .sum::<u64>();
            eprintln!(
                "[DISPATCH-SUMMARY]   receiver={} key={:#x} lambda_site={:?} callbacks={} caches={} cache_callbacks={} lambda_cache_callbacks={}",
                receiver.kind.label(),
                receiver.value,
                receiver.lambda_site,
                count,
                cache_ptrs,
                cache_callbacks,
                lambda_cache_callbacks,
            );
        }
    }
}

/// Capture a resolver decision. The dispatcher calls this only after the current callback has
/// installed its site context; no user workload identities are consulted outside the opt-in probe.
pub(crate) fn resolution_for_current(
    receiver_class: &str,
    implementation_class: &str,
    method: &str,
    descriptor: &str,
    leaf: Option<bool>,
    fill: &str,
    reason: &str,
) {
    if enabled() {
        note_resolution(
            receiver_class,
            implementation_class,
            method,
            descriptor,
            leaf,
            fill,
            reason,
        );
    }
}
