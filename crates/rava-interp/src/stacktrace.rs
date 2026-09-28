//! `Throwable.getStackTrace()` — capturing the frames, and rendering them as elements.
//!
//! Before this, `getStackTrace` answered a zero-length array and `fillInStackTrace` was a
//! no-op returning `this`, so **every stack trace this jar logs came out blank** and
//! `getStackTraceElement(0).getMethodName()` had nothing to return
//! (bench/ExcSem.java, `stack.nonempty` and `stack.method`).
//!
//! ## The frames were always there
//!
//! `frames::local()` is this thread's live frame registry, and each `Frame` carries the
//! `Arc<LoadedClass>` it is executing in plus a `*const Code` into that class. The method
//! NAME is recoverable without adding a single byte to the hot path: scan the frame's own
//! method table for the entry whose `code` pointer matches. That is one pointer comparison
//! per method AT THROW TIME and nothing per call — which is the whole reason this is
//! affordable at all.
//!
//! ## What is not available
//!
//! LINE NUMBERS. `Code::parse` skips the nested attributes, so there is no LineNumberTable
//! anywhere in the tree; elements report line −1 and `toString` renders `(Unknown Source)`,
//! which is exactly what the JVM prints for a class compiled without debug information.
//! Adding the table is a classfile-parser change and a per-class memory cost, and nothing
//! measured yet says it is worth either.
//!
//! ## Cost
//!
//! Java captures at CONSTRUCTION — by the time anyone asks, the frames are gone — so this
//! runs once per Throwable built, not once per trace read. `RAVA_NO_STACK_CAPTURE=1`
//! disables it, and `RAVA_STACK_DEPTH` caps the walk (default 64; the JVM's own default
//! `MaxJavaStackTraceDepth` is 1024, and a boot that builds exceptions in a loop should not
//! pay for depth nobody prints).

use crate::*;

/// Where a captured trace lives on the throwable: an array of `class\u{1}method` strings.
/// One string per frame rather than two arrays, because the pair is always read together
/// and a single `Value::Str` per frame is one allocation instead of two.
const STACK: &str = "$stack";
const SEP: char = '\u{1}';

fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_STACK_CAPTURE").is_none())
}

fn depth_cap() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RAVA_STACK_DEPTH").ok().and_then(|v| v.parse().ok()).unwrap_or(64)
    })
}

/// The method a frame is executing, by matching its `Code` pointer against the class's own
/// method table.
///
/// Returns `None` rather than guessing: a frame whose code pointer matches nothing is a
/// synthetic or native frame, and naming it wrongly is worse than leaving it out — a stack
/// trace is read by a human deciding where to look.
fn method_of(f: &Frame) -> Option<&'static str> {
    let class: &LoadedClass = &f.class;
    for m in class.file.methods.iter() {
        if let Some(code) = m.code.as_ref() {
            if std::ptr::eq(code as *const _, f.code) {
                // SAFETY: the name lives in the `LoadedClass` the loader owns for the
                // process's lifetime; the frame holds an `Arc` to it for at least this call.
                return Some(unsafe { std::mem::transmute::<&str, &'static str>(m.name.as_str()) });
            }
        }
    }
    None
}

/// Capture THIS thread's frames onto `h`, innermost first.
///
/// Idempotent in the way Java needs: `fillInStackTrace` re-captures, a constructor captures
/// once, and re-capturing simply overwrites. Silent no-op when disabled.
pub(crate) fn capture(vm: &Vm, h: u32) {
    if !enabled() {
        return;
    }
    let mut addrs: Vec<usize> = Vec::new();
    frames::local(&mut addrs);
    let cap = depth_cap();
    // Forensic opt-in for failures whose classfile has no LineNumberTable.
    let include_pc = std::env::var_os("RAVA_STACK_PC").is_some();
    let mut out: Vec<Value> = Vec::with_capacity(addrs.len().min(cap));
    for &p in addrs.iter().rev().take(cap) {
        // SAFETY: `local()` returns pointers to frames live on THIS thread's interpreter
        // stack, and nothing between that call and this read can pop them.
        let f = unsafe { &*(p as *const Frame) };
        let name = match method_of(f) {
            Some(name) => name,
            None if include_pc => "<unknown>",
            None => continue,
        };
        let cls = f.class.file.this_class.as_str();
        let name = if include_pc {
            format!("{name}@pc={}", f.pc)
        } else {
            name.to_owned()
        };
        out.push(Value::Str(format!("{cls}{SEP}{name}").into()));
    }
    if out.is_empty() {
        return;
    }
    let arr = vm.heap.alloc_values("Ljava/lang/String;", out);
    vm.heap.put_field(h, STACK, Value::Array(arr));
}

/// `Throwable.getStackTrace()` — the captured frames as `java/lang/StackTraceElement`
/// objects, or an empty array when nothing was captured (a throwable built before capture
/// existed, or one built with capture disabled).
pub(crate) fn elements(vm: &mut Vm, h: u32) -> Value {
    let frames_arr = match vm.heap.get_field(h, STACK) {
        Value::Array(a) => vm.heap.array_values(a).unwrap_or_default(),
        _ => Vec::new(),
    };
    // ROOTED (ledger rows 340/341). Each `alloc` below can collect, and every element built so
    // far lives only in this Rust local — invisible to the root walk, so the collector would free
    // elements this loop had already made. The buffer is PRE-SIZED and every slot is a valid
    // `Value` from the start, because `native_args_push` records its `(ptr, len)` ONCE: a growing
    // `Vec` leaves later elements unrooted and a reallocating one leaves the registration dangling.
    // The non-`Str` filter runs FIRST, so the rooted buffer's length is exact and never changes.
    // Truncating a REGISTERED buffer would leave the collector reading slots whose `Value`s have
    // been dropped, which is worse than the bug being fixed.
    let strs: Vec<crate::value::RStr> = frames_arr
        .into_iter()
        .filter_map(|v| match v {
            Value::Str(s) => Some(s),
            _ => None,
        })
        .collect();
    let n = strs.len();
    let mut out: Vec<Value> = vec![Value::Null; n];
    let base = crate::frames::native_args_push(out.as_ptr(), n);
    for (i, s) in strs.iter().enumerate() {
        let (cls, name) = s.split_once(SEP).unwrap_or((s.as_ref(), ""));
        let e = vm.heap.alloc("java/lang/StackTraceElement");
        // Guest-visible names are DOTTED — `getClassName` on the JVM answers
        // `java.lang.Foo`, and this string is printed straight into logs.
        vm.heap.put_field(e, "$declaringClass", Value::Str(cls.replace('/', ".").into()));
        vm.heap.put_field(e, "$methodName", Value::Str(name.into()));
        out[i] = Value::Ref(e);
    }
    // Still registered ACROSS `alloc_values`, which allocates: moving a `Vec` does not move its
    // heap buffer, so the registration stays valid for the copy it makes. Released immediately
    // after, with no allocation in between.
    let arr = vm.heap.alloc_values("Ljava/lang/StackTraceElement;", out);
    crate::frames::native_args_truncate(base);
    Value::Array(arr)
}

/// One rendered frame, in the JVM's own `at pkg.Class.method(Unknown Source)` form.
pub(crate) fn element_to_string(vm: &Vm, e: u32) -> String {
    let cls = match vm.heap.get_field(e, "$declaringClass") {
        Value::Str(s) => s.to_string(),
        _ => String::new(),
    };
    let name = match vm.heap.get_field(e, "$methodName") {
        Value::Str(s) => s.to_string(),
        _ => String::new(),
    };
    format!("{cls}.{name}(Unknown Source)")
}
