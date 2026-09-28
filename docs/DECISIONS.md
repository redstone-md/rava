# Decision log

The reasoning behind the choices that shaped rava, including the ones that turned out
to be wrong. This is not a spec. It's the "why did you do it that way" answers, kept
honest so the next person doesn't have to re-derive them.

## The moving GC ships behind a feature flag, not a switch

The engine started with a handle-based mark-sweep heap: objects live in a `Vec`,
references are indices, the GC marks and sweeps. It works and it's simple. The
problem is speed: indices mean every field access is a table lookup, and mark-sweep
fragments.

The moving generational GC (the arena) is the answer, but rewriting the live heap
model is the kind of change that breaks everything subtly and at once. So it went in
behind a `next-gen-gc` Cargo feature. Build without it and the engine is byte-for-byte
the old runtime. Build with it and every allocation, field op, and GC root path
switches to the arena.

This cost something. Half the codebase has two paths, and a `#[cfg]` on the wrong
line silently compiles out a fix. But the alternative was a months-long branch that
either lands all at once or not at all, and that branch tends not to land. Gating it
meant every increment was shippable and testable against the old behavior. The
default build never regressed because the default build never changed.

## Arena objects are raw low-address pointers, not handles

Once objects move, a reference has to survive the move. The handle model does this
for free, because the index doesn't change when the object relocates. Raw pointers don't:
the collector has to find and rewrite every one.

We went with raw pointers anyway, because the win is direct memory access. An arena
object is a header plus inline cells, and a field read is a `load` at a constant
offset. No indirection.

The hard part was telling a raw arena pointer apart from a heap handle at runtime,
since both have to coexist during the migration and both are `i64` in compiled code.
The trick: reserve the arena's memory low, under 2^40, and tag handles in the high
bits. Then "is this an arena pointer" is one shift and compare — `addr >> 40 == 0`.
It's a real constraint (the OS mapping has to land low, and we exit hard if it
can't), but it makes the discriminator a single instruction on the hot path, which
is where it runs millions of times.

## Java strings are arena objects of an empty-ref-mask class

Strings were the last thing allocating through the slow path, and StringOps was
stuck at ~270x the JVM. The obvious move is to make strings real arena objects. The
non-obvious part is doing it without a new `Value` enum variant (which would ripple
through hundreds of match arms) and without touching the GC core.

The solution is almost a trick: register strings as a normal arena class whose
reference mask is empty. The Cheney collector already copies any object by its size
and traces its reference cells. An empty mask means it copies the bytes and traces
nothing, which is right for a string, where the inline bytes must not be mistaken for
pointers. No special case in the collector, no new variant. A string is just a
`Value::Obj` whose class happens to be the string class, and a pile of dual-path
helpers make it behave identically to the old `Value::Str` everywhere it's read.

## Concurrency is a global interpreter lock plus green threads

The honest reason rava has a GIL is the same as CPython's: a single writer is the
only way the shared heap and the green-thread scheduler stay correct without auditing
every data structure for thread safety. Real OS threads exist (Minecraft spawns
them), and they run under the lock, handing off at safepoints.

This is a deliberate ceiling. It means real bytecode execution is serial, and the
multi-core work doesn't change that for a Minecraft tick. The parallel forker gets
around the GIL only because its workers run provably isolated code (pure arithmetic
into their own nurseries), so they can drop the lock entirely. The moment a worker
would need shared state, it bails back under the GIL. The GIL isn't a thing we're
proud of. It's the thing that let the rest work.

## The auto-parallelizer is fail-closed with a serial fallback

Auto-parallelizing a loop is only correct if the iterations are genuinely
independent. Get that wrong and you get a data race that produces a different answer
every run. So the analyzer is conservative to the point of paranoia: any control flow
it can't track, any store to shared state, any call it can't prove pure, and the loop
is rejected. It would rather miss a parallelizable loop than parallelize an unsafe
one.

And even when it accepts a loop, the generated code keeps a serial path. If a worker
overflows its nursery mid-flight (which it can't fix on its own, because collecting
needs the lock), it aborts the whole parallel region and the driver re-runs the loop
sequentially. The fast path is allowed to give up. Correctness comes from the fact
that giving up always lands on a path that's known to work. Every result is checked
bit-for-bit against the serial fold, including the overflow case.

## Scalar replacement runs in the parallel body, not the general JIT

Escape analysis says which objects never leave a method. Scalar replacement then
erases them: fields become registers and the allocation disappears. The textbook place
to do this is the main code generator, for every method.

We didn't, on purpose. The main generator has control flow, which means an object
might be virtual on one path and real on another, and reconciling that at a branch
merge is where scalar replacement gets genuinely hard and easy to get wrong. The
data-parallel loop body is straight-line, one basic block with no merges. So scalar
replacement there is sound by construction: a virtual object is a compile-time map of
fields to SSA values, and there's no second path to reconcile it against.

It's a smaller win than general SROA would be. But it's the win that mattered, because
it's what lets a loop that allocates an object every iteration run on the parallel
workers at all. Without it, the allocations fill the worker nurseries and the region
aborts back to serial. With it, the workers never touch the heap.

## Measure before you believe, even your own optimization

The one that stings. The plan to fix StringOps was to make the concat loop compile
instead of staying interpreted. It compiled. It got 15% *slower*, from 268x to 308x.

The reason, once measured: the loop was callback-bound, and compiling it didn't
remove the callbacks, it just added a machine-code-to-Rust boundary in front of each
one, with spill and reload on both sides. The interpreted version was cheaper because
it never crossed that boundary. The optimization was reverted the same day.

The rule that came out of it: build the thing, measure it, and let the measurement
overrule the theory even when the theory is yours. The arena string subsystem, which
actually worked, came from the opposite direction, by making the operations native so
there was no callback left to be the bottleneck. We only knew that was the right
direction because the first guess was wrong and we believed the number instead of the
plan.

## The engine never adapts to a specific jar

The Minecraft server is the health check. It is not the target. Every native is
generic JDK behavior, every fix is a fix to the spec implementation, and there is no
"if class name contains Minecraft" anywhere. The temptation is real — a hardcoded
shortcut would boot the server faster today. It would also make rava a Minecraft
emulator instead of a JVM, and the whole point is the second thing.

## Two levers that remove the same cost: count the population, don't sum the deltas

Twice now, two parallel tracks have attacked `std::thread::local::LocalKey::with` — a
top-three RIP leaf in every boot window — and arrived with separate flags and separate
claims. `RAVA_TLS_CLUSTER` grouped each set of per-thread caches into one struct behind
an inlined pointer cursor. `RAVA_DISPATCH_FASTFRAME` took the per-thread JIT context once
per warm direct call and did the whole synthetic frame's bookkeeping through that handle.
Both named `Vm::jit_direct_invoke` in their commit messages. Neither could be trusted to
add to the other, and on a box where a boot's wall varies 2x, no wall A/B was ever going
to separate them.

What settled it was counting the population instead of timing the change. `RAVA_TLS_STAT`
tallies the crossings each cluster actually performs, on the cluster's own disabled arm,
so the counter never touches the path that would ship. Over a real boot to `Done`: 1.654G
crossings, of which `frames` 1.242G, `tlcache` 0.309G and `owner_bits` 0.103G. Re-run with
the fast frame on: the same boot doing the same work (222,173,829 direct hits against
222,173,810), `frames` down to 0.607G. So the fast frame removes 635.8M crossings — 38.4%
of the whole population, all of it inside one cluster, and distributed nothing like the
profile suggested: 13.0% of blocks A+B, **1.0% of block C**, 55.0% of the Done window.

That answers both questions at once. The two are not substitutes to choose between — the
cursor covers the whole population in every window, the fast frame removes a third of it
outright, and the combined arm beats either — but summing their deltas double-counts
38.4%, and the fast frame is worth almost nothing in block C, which is the window where
`LocalKey::with` is the single largest leaf. One is a lever with two sides. The other
turned out not to be a lever at all, just a better shape for one function, so it lost its
flag rather than its code: an OFF arm nobody would ever select was four `is_null()`
diamonds of dead weight in the hottest function on the map.

The general rule: when two changes target the same named cost, price the COUNT of the
thing being removed before pricing either change. A count is immune to the contention that
makes a wall unusable here, it is additive in a way deltas are not, and it gives the
overlap exactly. A profile row tells you what a cost is worth; only a census tells you
whose it is.
