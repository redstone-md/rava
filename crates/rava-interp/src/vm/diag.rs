//! Reading a live stack: traces, exceptions, and the classes a frame mentions.

use crate::*;

impl Vm {
    /// The singleton `java/lang/Boolean` heap object for `b`. Boxed booleans are
    /// real objects so `instanceof Boolean` separates them from boxed ints (the
    /// `$value` field carries the 0/1). Cached so identity (`==`) holds.
    pub(crate) fn boolean_obj(&mut self, b: bool) -> Value {
        let slot = b as usize;
        if let Some(h) = self.bool_cache[slot] {
            return Value::Ref(h);
        }
        let h = self.heap.alloc("java/lang/Boolean");
        self.heap.put_field(h, "$value", Value::Int(b as i32));
        Self::debug_assert_not_gilfree("bool_cache");
        self.bool_cache[slot] = Some(h);
        Value::Ref(h)
    }

    /// A boxed `Character`/`Short`/`Byte` for `n` — a REAL heap object of that class carrying
    /// the NARROWED payload in `$value`, exactly as [`Vm::boolean_obj`] does for `Boolean`.
    ///
    /// ⭐**WHY A BOX AND NOT `Value::Int`.** `Value::Int` is this engine's representation of a
    /// boxed `Integer`, so a `Character` kept as one answered `java.lang.Integer` from
    /// `getClass()` and rendered as its CODE POINT wherever the object was printed — including
    /// `Recipe.CODEC`'s `Map<Character, Ingredient>`, which encoded `{"key":{"35":…}}` against
    /// the JVM's `{"key":{"#":…}}`. The `Value` variant names four types and there are seven, so
    /// the three it cannot name have to be objects. See [`crate::boxid`] for the full mechanism
    /// and the `RAVA_NO_BOXID` kill switch.
    ///
    /// INTERNED on the JDK's own ranges ([`crate::boxid::BoxCache`]) so `valueOf` identity is the
    /// reference implementation's; outside them a fresh object per call, which is also what the
    /// JDK does. Interned handles are GC roots (see `Vm::gc`).
    pub(crate) fn narrow_box(&mut self, kind: crate::boxid::Kind, n: i32) -> Value {
        let n = kind.narrow(n);
        if let Some(h) = self.box_cache.get(kind, n) {
            crate::boxid::note_mint(kind, true);
            return Value::Ref(h);
        }
        let h = self.heap.alloc(kind.class());
        self.heap.put_field(h, "$value", Value::Int(n));
        // Only the INTERNED range touches the shared table, and only that range needs the
        // GIL assertion `bool_cache` carries for the same reason: an out-of-range box is
        // handed straight back and is reachable from the caller's own operand stack.
        let interned = self.box_cache.slot(kind, n).is_some();
        if interned {
            Self::debug_assert_not_gilfree("box_cache");
            self.box_cache.put(kind, n, h);
        }
        crate::boxid::note_mint(kind, interned);
        Value::Ref(h)
    }

    /// A narrow box read back as the primitive it holds, or `None` for anything that is not one
    /// — so a caller can NORMALISE an operand and pay nothing for the overwhelmingly common
    /// case.
    ///
    /// ⭐**THIS IS THE OTHER HALF OF `narrow_box`, AND IT IS THE HALF THAT KEEPS THE ENGINE
    /// WORKING.** Boxing `Character`/`Short`/`Byte` (`crate::boxid`) turns a `Value::Int`
    /// operand into a `Value::Ref`, and this engine is full of small pure helpers that take a
    /// `&Value` and NO `&Vm` — `scratch::natural_cmp`, `natural_hr`, `natural_tag_hr`,
    /// `map_method::natural_cmp` — so they cannot unbox and cannot be given the heap without
    /// threading it through a dozen signatures. Their contract is "decide from the `Value`
    /// alone", and the answer is to hand them a `Value` that still says what it is.
    ///
    /// ⛔The cost of not doing this, measured: `guava`'s `ArrayBasedEscaperMap` calls
    /// `Collections.max(map.keySet())` over a `Map<Character, String>`, `natural_cmp` fell to
    /// `a.display().cmp(&b.display())` — two HANDLE renderings — and answered a `max` of `'"'`
    /// (34) for a set containing `'\\'` (92). The array sized from it was 35 long and the fill
    /// loop then wrote index 92: `ArrayIndexOutOfBoundsException` out of
    /// `LockComponentPredicateFix.<clinit>`, which took down MINECRAFT BOOTSTRAP — every
    /// `net.minecraft` probe and three of the four PGO gates, from one comparison.
    #[inline]
    pub(crate) fn unbox_narrow(&self, v: &Value) -> Option<Value> {
        if !matches!(v, Value::Ref(_)) || !crate::boxid::enabled() {
            return None;
        }
        crate::natives::boxed_narrow(self, Some(v)).map(|(_, n)| Value::Int(n))
    }

    /// LIGHT backtrace from the live frame registry (class + pc, innermost first) —
    /// no `call_stack` bookkeeping, so it needs no per-call String allocs and can be
    /// read at a throw site without the timing/allocation distortion (and resulting
    /// OOM in pathological toString recursion) that populating `call_stack` causes.
    /// Method names aren't stored on `Frame`, so frames show as `class pc=N`.
    pub(crate) fn frame_trace(&self) -> String {
        let mut s = String::new();
        for &p in frames::all().iter().rev().take(8) {
            let f = unsafe { &*(p as *const Frame) };
            s.push_str(&format!("  at {} pc={}\n", f.class.file.this_class, f.pc));
        }
        s
    }

    /// Diagnostic (Wall #11): render a Throwable value (heap `Ref`) plus its
    /// `$cause` chain as indented `class: message` lines, outermost first. Reads
    /// only the synthetic `$message`/`$cause` fields — no `toString` bytecode, so
    /// no allocation/recursion at the throw site. An arena `Obj` cause (rare —
    /// exceptions are no-bytecode JDK classes, so heap handles) terminates the
    /// walk with a tag rather than being chased.
    pub(crate) fn throwable_chain(&self, v: &Value) -> String {
        let mut out = String::new();
        let mut cur = v.clone();
        for depth in 0..8 {
            let h = match cur {
                Value::Ref(h) => h,
                Value::Null => break,
                other => {
                    out.push_str(&format!("{}<non-heap throwable: {}>\n", "  ".repeat(depth), other.display()));
                    break;
                }
            };
            let cls = self.heap.class_of(h).unwrap_or("?").replace('/', ".");
            let msg = match self.heap.get_field(h, "$message") { Value::Str(s) => s.to_string(), _ => String::new() };
            out.push_str(&format!("{}{cls}: {msg}\n", "  ".repeat(depth)));
            match self.heap.get_field(h, "$cause") {
                Value::Ref(c) if c != h => cur = Value::Ref(c),
                Value::Null => break,
                other => { out.push_str(&format!("{}caused by {}\n", "  ".repeat(depth + 1), other.display())); break; }
            }
        }
        out
    }

    /// Render the current call stack top-first (for throw/backtrace traces).
    pub(crate) fn backtrace(&self) -> String {
        crate::vm::thread_context::with_call_stack(
            &self.call_stack,
            crate::vm::thread_context::render_call_stack,
        )

    }
    pub(crate) fn print(&mut self, line: &str, newline: bool) {
        if newline {
            println!("{line}");
            self.output.push_str(line);
            self.output.push('\n');
        } else {
            print!("{line}");
            self.output.push_str(line);
        }
    }

    /// Class names of the live interpreter frames, outermost first — the real
    /// call stack for caller introspection (sun.reflect.Reflection /
    /// StackWalker). Built from `frame_ptrs` (always maintained, unlike the
    /// trace-gated `call_stack`).
    pub(crate) fn live_frame_classes(&self) -> Vec<String> {
        // This thread's recursive Rust-stack frames (always pushed by `opcodes::run`,
        // bottom→top). In pure recursive mode this IS the whole virtual call chain.
        let mut fp = Vec::new();
        frames::local(&mut fp);
        let local: Vec<String> = fp
            .iter()
            .map(|&p| {
                // SAFETY: every pointer refers to a Frame live on this thread's Rust
                // call stack (pushed in `run`, popped on return).
                let f = unsafe { &*(p as *const Frame) };
                f.class.file.this_class.to_string()
            })
            .collect();

        // Trampoline (execute_stackful / RAVA_STACKFUL): the OUTER frames live in the
        // explicit `exec_frames` Vec, not on the Rust stack. `drive_stack` runs the
        // top frame via `opcodes::run`, which ALSO pushes it to LOCAL — so the
        // currently-running frame is `exec_frames.last()` AND `local[0]`, and any
        // recursive sub-calls it makes (a `<clinit>` chain via `ensure_init`, native
        // re-entry — e.g. log4j's StackLocator) stack onto `local[1..]`. The complete
        // chain is therefore `exec_frames ++ local[1..]`, which reconstructs exactly
        // what a pure-recursive run would have on LOCAL — so getCallerClass depth math
        // is identical in both modes. Without this, the trampolined outer frames
        // (Main → MinecraftServer → DispenserRegistry) are missing and a deep
        // getCallerClass(depth) underflows to null ("No class provided").
        if let Some(exec_ptr) = frames::exec_current(self as *const Self as usize) {
            // SAFETY: ExecScope registers this vector only while its driver is
            // active on this thread, and the scope ends before the vector moves.
            let exec = unsafe { &*(exec_ptr as *const Vec<(Frame, Arc<str>, Arc<str>)>) };
            let mut out: Vec<String> = exec
                .iter()
                .map(|(f, _, _)| f.class.file.this_class.to_string())
                .collect();
            // Drop local[0]: it is the same frame as exec.last() (pushed to both by
            // `drive_stack`'s `opcodes::run`). Guard on a name match so a stale/empty
            // exec pointer can't mis-dedup.
            let tail = match (out.last(), local.first()) {
                (Some(a), Some(b)) if a == b => &local[1..],
                _ => &local[..],
            };
            out.extend_from_slice(tail);
            return out;
        }
        local
    }

    /// Human-readable class+message of an uncaught guest Throwable, for the CLI's
    /// top-level report (`VmError::Throw`'s Display is anonymous — "uncaught Java
    /// exception" with no identity made every top-escape a fresh hunt).
    pub fn describe_error(&self, e: &VmError) -> Option<String> {
        let VmError::Throw(v) = e else { return None };
        Some(match v {
            Value::Ref(h) => {
                let cls = self.heap.class_of(*h).unwrap_or("?").to_string();
                match self.heap.get_field(*h, "$message") {
                    Value::Str(s) => format!("{cls}: {s}"),
                    _ => cls,
                }
            }
            other => format!("{other:?}"),
        })
    }

    /// ⚠**`#[inline(never)]` DELIBERATELY** — row 397. At opt-level 3 LLVM folds this into
    /// `opcodes::run`, and an inlined body's locals are reserved by `run`'s PROLOGUE, which
    /// EVERY interpreted call pays before the callee's first opcode. `run` measured a **2136-byte**
    /// stack frame and a framed call ~6.7 KB across the path, against a measured frame cost of
    /// **160 ns vs HotSpot's 7.2**. Keeping this out of line trades a call on a cold path for a
    /// smaller prologue on the hot one.
    #[inline(never)]
pub(crate) fn make_exception(&mut self, class: &str, message: &str) -> Value {
        EXC_MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Diagnostic: surface every VM-made exception with its Java backtrace
        // (the program may swallow it — e.g. MC catches Throwable at top level
        // and its logger is a no-op here, hiding the root cause entirely).
        if self.trace.throw {
            eprintln!("[THROW] {class}: {message}\n{}", self.backtrace());
        } else if self.trace.fatal && class.ends_with("Error") {
            // FATAL-THROW INTERCEPT (Rust path): a VM-synthesised `*Error`
            // (UnsatisfiedLinkError / NoClassDefFoundError …) is the usual boot
            // killer MC's top-level catch swallows. Low-volume (Error-only), so the
            // backtrace here doesn't flood/distort timing like RAVA_TRACE_THROW.
            eprintln!("[FATAL-THROW] {class}: {message}\n{}", self.frame_trace());
        } else if self.trace.athrow {
            // Lightweight variant: no backtrace (call_stack is not populated).
            eprintln!("[THROW-MAKE] {class}: {message}");
        }
        let h = self.heap.alloc(class);
        // A VM-RAISED exception gets the same treatment as a guest-constructed one: the
        // guest catches it, logs it, and its trace should name where it came from.
        crate::stacktrace::capture(self, h);
        // "" is how every VM-raised site here spells "this exception has no
        // message", and on HotSpot that is a NULL message: `getMessage()` returns
        // null and `toString()` prints the class name ALONE. Storing an empty
        // Str instead made `getMessage()` return "" (so `if (e.getMessage() !=
        // null)` took the wrong branch) and `toString()` print a dangling
        // "java.lang.StackOverflowError: ". A guest-supplied empty message cannot
        // reach here — that path is the `<init>` service in natives/misc.rs.
        if !message.is_empty() {
            self.heap
                .put_field(h, "$message", Value::Str(message.into()));
        }
        Value::Ref(h)
    }
    /// Phase 1 tracer: exception allocation on `&SharedVm`.
    ///
    /// The body uses only shared heap operations and walks this thread's frame
    /// registry for stack capture. `SharedVm == Vm` at Phase 1, so the
    /// `&mut self` origin stays byte-identical; a later phase can move object
    /// allocation through `ThreadContext.heap_view` and route roots through
    /// worker-local frame state.
    pub(crate) fn make_exception_shared(&self, class: &str, message: &str) -> Value {
        EXC_MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let trace_throw = self.trace.throw;
        let trace_fatal = self.trace.fatal;
        let trace_athrow = self.trace.athrow;
        if trace_throw {
            eprintln!("[THROW] {class}: {message}\n{}", self.backtrace());
        } else if trace_fatal && class.ends_with("Error") {
            eprintln!("[FATAL-THROW] {class}: {message}\n{}", self.frame_trace());
        } else if trace_athrow {
            eprintln!("[THROW-MAKE] {class}: {message}");
        }
        let h = self.heap.alloc(class);
        crate::stacktrace::capture(self, h);
        if !message.is_empty() {
            self.heap.put_field(h, "$message", Value::Str(message.into()));
        }
        Value::Ref(h)
    }
    /// Phase 3 tracer: exception allocation through `ThreadContext.heap_view`.
    ///
    /// Probes the thread-local TLAB window (a bump cursor) instead of casting
    /// `&SharedVm` to `&mut Vm` for `Heap::alloc`. On success the window has
    /// reserved space for the object body; the handle is still issued via the
    /// shared table (`heap.alloc`). On exhaustion the window is synced back
    /// to the global TLAB cursor and we fall back to `make_exception_shared`
    /// (which will trigger GC/refill internally).
    pub(crate) fn make_exception_ctx(
        &self,
        ctx: &mut crate::vm::thread_context::ThreadContext,
        class: &str,
        message: &str,
    ) -> Value {
        EXC_MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let trace_throw = self.trace.throw;
        let trace_fatal = self.trace.fatal;
        let trace_athrow = self.trace.athrow;
        if trace_throw {
            eprintln!("[THROW] {class}: {message}\n{}", self.backtrace());
        } else if trace_fatal && class.ends_with("Error") {
            eprintln!("[FATAL-THROW] {class}: {message}\n{}", self.frame_trace());
        } else if trace_athrow {
            eprintln!("[THROW-MAKE] {class}: {message}");
        }
        let size = 64usize;
        let align = 8usize;
        if ctx.heap_view.alloc_raw(size, align).is_some() {
            let h = self.heap.alloc(class);
            crate::stacktrace::capture(self, h);
            if !message.is_empty() {
                self.heap.put_field(h, "$message", Value::Str(message.into()));
            }
            Value::Ref(h)
        } else {
            ctx.heap_view.sync_to_tlab();
            self.make_exception_shared(class, message)
        }
    }

    /// Allocate a `java/lang/Class` instance representing `binary_name`.
    pub(crate) fn make_class(&mut self, binary_name: &str) -> Value {
        if let Some(&h) = self.class_objects.get(binary_name) {
            return Value::Ref(h);
        }
        let h = self.heap.alloc("java/lang/Class");
        self.heap
            .put_field_syn(h, crate::heap::Syn::Name, Value::Str(binary_name.into()));
        // FROZEN contract (docs/gil-free-vm-layout.md): the class-object intern map is warmed
        // before a region; a worker interning a new Class handle mid-region would race.
        parallel::assert_not_gilfree("class_objects");
        self.class_objects.insert(binary_name.to_string(), h);
        Value::Ref(h)
    }
}
