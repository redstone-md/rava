# RAVA compatibility roadmap (SEPARATE from performance work)

Tracks SUPPORTED vs UNSUPPORTED guest Java features. Distinct axis from the
performance program — do not conflate a compatibility gap with a perf result.

## Currently UNSUPPORTED (observed)
- Guest multithreading / java.util.concurrent coordination: `new Thread().start()`
  with CountDownLatch.await → UnsatisfiedLinkError (corpus/conformance/Concurrency.java,
  OFF==ON). Threads, synchronization, latches, barriers, futures across guest
  threads are not runnable.

## Known interpreter-only defects (JIT=1 correct)
- Reflection: Method.invoke of an INSTANCE method with a null receiver stops early
  on RAVA_JIT=0 (JIT=1 OFF & ON and JVM correct). corpus/conformance/Reflect.java.

## Supported subset (verified JVM-equivalent, JIT=1)
Single-threaded: mono/poly/mega virtual+interface dispatch, default methods,
lambdas + method refs, invokedynamic string concat, boxing + primitive streams,
reflection (Method.invoke/Constructor.newInstance on JIT=1), class init/late
linkage, exceptions across dispatch, native string boundary. See
corpus/conformance/ + PERFORMANCE_CONTRACT.md.

## Priority (compatibility, not performance)
Guest thread model + java.util.concurrent primitives is a large runtime effort
(GIL interaction, real OS-thread scheduling). Track here; NOT part of the perf
program.

## Throwable.getStackTrace returns an empty array (found 2026-08-16, bench/ExcSem.java)

`new Throwable().getStackTrace()` answers a zero-length array, so every stack trace this
jar logs is blank where it should name a method — and `getMethodName()` on element 0 has
nothing to return. `natives/misc.rs` serves `getStackTrace` with an empty allocation and
`fillInStackTrace` as a no-op that returns `this`.

**The frames are available.** `frames::all()` is a live registry of `Frame` pointers, each
holding `class: Arc<LoadedClass>` and `code: *const Code`; `Vm::frame_trace` already
renders `class pc=N` from it at throw sites. The method NAME is recoverable without adding
anything to the hot path: scan the frame's own class method table for the entry whose
`code` pointer matches, which costs one pointer comparison per method AT THROW TIME and
nothing per call. Line numbers are NOT available — `Code::parse` skips nested attributes,
so there is no LineNumberTable and elements would read as `(Unknown Source)`, which is what
the JVM prints for the same absence.

**Why it is parked rather than half-built.** The elements have to be real
`java/lang/StackTraceElement` objects with `getClassName` / `getMethodName` / `toString`,
and serving a new owner means touching `dispatch_owner`'s `owner_bits` plan — the cascade
gate that decides which sub-dispatchers an owner may reach. That is a real integration, not
an afternoon, and it advances compatibility rather than the wall/Done goal. It is written
down here so the next session starts from the mechanism instead of rediscovering it.

Cost to watch when it is built: capture happens per Throwable CONSTRUCTION (Java's
semantics — the frames are gone by the time anyone asks), so it wants a
`RAVA_NO_STACK_CAPTURE` flag and a count of how many Throwables a boot builds before it
goes default-on. `EXC_MADE` counts only VM-synthesised ones, not guest `new`.
