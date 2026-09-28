//! Opt-in route attribution for one `rava_invoke` callback at a time.
//!
//! A thread-local stack keeps callbacks re-entered from compiled code independent: an inner
//! invocation can record its route without replacing the outer invocation's attribution.
use std::cell::RefCell;

crate::diag_counter!(pub aot_guard_gilfree, "aot_route", "aot.guard_gilfree", "AOT requests refused on GIL-free workers");
crate::diag_counter!(pub aot_guard_clinit, "aot_route", "aot.guard_clinit", "AOT requests refused before class initialization completes");
crate::diag_counter!(pub aot_guard_native_synth, "aot_route", "aot.guard_native_synth", "AOT requests refused for native or synthetic owners");
crate::diag_counter!(pub aot_cache_ready, "aot_route", "aot.cache_ready", "AOT cache hits with a linked function");
crate::diag_counter!(pub aot_cache_declined, "aot_route", "aot.cache_declined", "AOT cache hits containing a negative result");
crate::diag_counter!(pub aot_cache_miss, "aot_route", "aot.cache_miss", "AOT cache misses");
crate::diag_counter!(pub aot_admission_wait, "aot_route", "aot.admission_wait", "Cold AOT requests below the hotness threshold");
crate::diag_counter!(pub aot_link_attempt, "aot_route", "aot.link_attempt", "Cold AOT link attempts after admission");
crate::diag_counter!(pub aot_link_no_object, "aot_route", "aot.link_no_object", "Prepared AOT objects absent from the prepared directory");
crate::diag_counter!(pub aot_link_decode_fail, "aot_route", "aot.link_decode_fail", "Prepared AOT objects that failed decoding");
crate::diag_counter!(pub aot_link_bind_fail, "aot_route", "aot.link_bind_fail", "AOT objects rejected during relocation or constant binding");
crate::diag_counter!(pub aot_link_read_fail, "aot_route", "aot.link_read_fail", "Prepared AOT object read failures other than not found");
crate::diag_counter!(pub aot_link_unsupported, "aot_route", "aot.link_unsupported", "Cold AOT links declined by structural or translator coverage");
crate::diag_counter!(pub aot_link_error, "aot_route", "aot.link_error", "Cold AOT link errors without a more specific reason");
crate::diag_counter!(pub aot_pack_entered, "aot_route", "aot.pack_entered", "AOT calls entering argument layout lookup and packing");
crate::diag_counter!(pub aot_pack_layout_missing, "aot_route", "aot.pack_layout_missing", "AOT calls whose linked cache layout disappeared before packing");
crate::diag_counter!(pub aot_pack_completed, "aot_route", "aot.pack_completed", "AOT calls completing argument packing");
crate::diag_counter!(pub aot_execute_entered, "aot_route", "aot.execute_entered", "AOT calls entering generated machine code");
crate::diag_counter!(pub route_calls, "aot_route", "route.calls", "rava_invoke callbacks with route attribution enabled");
crate::diag_counter!(pub route_jit_direct, "aot_route", "route.jit_direct", "Terminal direct-JIT routes");
crate::diag_counter!(pub route_fastpath, "aot_route", "route.fastpath", "Terminal specialized or memo fastpaths");
crate::diag_counter!(pub route_aot_exec, "aot_route", "route.aot_exec", "Terminal AOT machine-code routes");
crate::diag_counter!(pub route_jit_entry, "aot_route", "route.jit_entry", "Terminal normal JIT-tier routes");
crate::diag_counter!(pub route_interpreted, "aot_route", "route.interpreted", "Terminal interpreter routes");
crate::diag_counter!(pub route_general_vm, "aot_route", "route.general_vm", "Terminal general VM routes not attributed to a compiled tier");
crate::diag_counter!(pub route_callback_early, "aot_route", "route.callback_early", "rava_invoke callbacks refused or exited before VM dispatch");
crate::diag_counter!(pub route_aot_declined, "aot_route", "route.aot_declined", "AOT stage declines that continued into a fallback tier");
crate::diag_counter!(pub aot_direct_static_attempt, "aot_route", "aot.direct_static_attempt", "Opt-in Linux static calls checked for a cached AOT target");
crate::diag_counter!(pub aot_direct_static_hit, "aot_route", "aot.direct_static_hit", "Linux static calls dispatched directly to an already-linked AOT target");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Route {
    DirectJit,
    Fastpath,
    AotExecuted,
    JitEntry,
    Interpreted,
    GeneralVm,
    CallbackEarly,
}

#[derive(Default)]
struct RouteStack(Vec<Option<Route>>);

impl RouteStack {
    fn push(&mut self) {
        self.0.push(None);
    }

    fn mark(&mut self, route: Route) {
        if let Some(current) = self.0.last_mut() {
            *current = Some(route);
        }
    }

    fn mark_if_unset(&mut self, route: Route) {
        if let Some(current) = self.0.last_mut() {
            current.get_or_insert(route);
        }
    }

    fn pop(&mut self) -> Option<Route> {
        self.0.pop().flatten()
    }

    fn finish(&mut self) -> Route {
        self.pop().unwrap_or(Route::GeneralVm)
    }
}

thread_local! {
    static ROUTES: RefCell<RouteStack> = RefCell::new(RouteStack::default());
}

/// Scope the terminal outcome to a single `rava_invoke` call. Inert unless the route category is
/// explicitly selected, so normal runs do not touch the thread-local stack.
pub(crate) struct InvokeScope {
    active: bool,
}

impl InvokeScope {
    pub(crate) fn enter() -> Self {
        let active = route_calls().armed();
        if active {
            ROUTES.with(|routes| routes.borrow_mut().push());
            route_calls().inc();
        }
        Self { active }
    }
}

impl Drop for InvokeScope {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let route = ROUTES.with(|routes| routes.borrow_mut().finish());
        match route {
            Route::DirectJit => route_jit_direct().inc(),
            Route::Fastpath => route_fastpath().inc(),
            Route::AotExecuted => route_aot_exec().inc(),
            Route::JitEntry => route_jit_entry().inc(),
            Route::Interpreted => route_interpreted().inc(),
            Route::GeneralVm => route_general_vm().inc(),
            Route::CallbackEarly => route_callback_early().inc(),
        }
    }
}

#[inline]
fn mark(route: Route) {
    if !route_calls().armed() {
        return;
    }
    ROUTES.with(|routes| routes.borrow_mut().mark(route));
}

#[inline]
pub(crate) fn direct_jit() {
    mark(Route::DirectJit);
}

#[inline]
pub(crate) fn fastpath() {
    mark(Route::Fastpath);
}

#[inline]
pub(crate) fn aot_executed() {
    mark(Route::AotExecuted);
}

#[inline]
pub(crate) fn jit_entry() {
    mark(Route::JitEntry);
}

#[inline]
pub(crate) fn interpreted() {
    mark(Route::Interpreted);
}

#[inline]
pub(crate) fn general_vm_if_unset() {
    if !route_calls().armed() {
        return;
    }
    ROUTES.with(|routes| routes.borrow_mut().mark_if_unset(Route::GeneralVm));
}
#[inline]
pub(crate) fn callback_early() {
    if !route_calls().armed() {
        return;
    }
    ROUTES.with(|routes| routes.borrow_mut().mark_if_unset(Route::CallbackEarly));
}

#[inline]
pub(crate) fn aot_declined() {
    route_aot_declined().inc();
}

#[cfg(test)]
mod tests {
    use super::{Route, RouteStack};

    #[test]
    fn nested_routes_restore_the_outer_attribution() {
        let mut routes = RouteStack::default();
        routes.push();
        routes.mark(Route::DirectJit);
        routes.push();
        routes.mark(Route::AotExecuted);

        assert_eq!(routes.pop(), Some(Route::AotExecuted));
        assert_eq!(routes.pop(), Some(Route::DirectJit));
        assert_eq!(routes.pop(), None);
    }

    #[test]
    fn unmarked_route_uses_general_vm_fallback() {
        let mut routes = RouteStack::default();
        routes.push();

        assert_eq!(routes.finish(), Route::GeneralVm);
    }

    #[test]
    fn route_scope_is_inert_when_not_selected() {
        if std::env::var_os("RAVA_DIAG_AOT_ROUTE").is_some()
            || std::env::var_os("RAVA_DIAG").is_some()
        {
            return;
        }
        let scope = super::InvokeScope::enter();
        assert!(!scope.active);
        assert!(super::ROUTES.with(|routes| routes.borrow().0.is_empty()));
    }
    #[test]
    fn route_scope_arms_from_the_dedicated_flag() {
        if std::env::var("RAVA_DIAG_AOT_ROUTE").as_deref() != Ok("1") {
            return;
        }
        assert!(super::route_calls().armed());
        let scope = super::InvokeScope::enter();
        assert!(scope.active);
        assert_eq!(super::ROUTES.with(|routes| routes.borrow().0.len()), 1);
        drop(scope);
        assert!(super::ROUTES.with(|routes| routes.borrow().0.is_empty()));
    }
}
