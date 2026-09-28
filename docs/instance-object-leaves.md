# Statically bound instance object leaves

The packed release's 30-second BootstrapBench diagnostic observed 18157228 AOT
frames but only 1631 cold links. Cold linking took 132223700 ns inclusive:
key computation 26535200 ns, reads 91486300 ns, decode 866700 ns, binding
11334300 ns. Sampled packing estimated 1.916 seconds. The diagnostic overlapped
a correctness gate and intentionally exited at 30 seconds; it is not a boot
timing or whole-process CPU decomposition. Evidence is under the archive server
root, `aot-costs-449a718716b1488cb57201f5710351c3`.

Extend the existing same-class object-leaf compiler to instance methods only
when private, final, or declared by a final class. These immutable bytecode
properties establish the target without receiver-history specialization.
Private methods support invokespecial and invokevirtual (as used by modern
javac); other non-overridable methods use invokevirtual. Preserve invocation null checks even when the method does not
dereference this. Keep native, synchronized, abstract, unsupported field effects
and unknown dispatch on the general path. Include class access flags in the
artifact identity because final-class proof now affects generated code.
The private-target selection rule is specified in
[JVMS 5.4.6](https://docs.oracle.com/javase/specs/jvms/se25/html/jvms-5.html#jvms-5.4.6).

Verify actual machine execution with aliased receivers and faults before/after
effects, dispatch rejection, class-finality key invalidation and moving GC.
Full server measurements are still required; no wall improvement is claimed.

58 AOT unit tests pass, including linked machine code for final/private/final-class
targets, null invocation without a body dereference, the original invocation kind
on secondary-receiver faults, and reference forwarding after a following moving
collection. The initial new unit harness omitted the existing outer null-fault
callback and crashed by calling address zero; the harness now binds and checks
that callback. No runtime null-guard bypass was introduced to make the test pass.

The rebuilt release passes the extended ObjectLeafSem against Java with JIT=0.
Both instance/private callers have confirmed prepared entries; their non-null
leaf bodies do not enter the general AOT runner. Fault cases preserve the first
write before a null second receiver and reject a null invocation receiver.
Evidence: `%TEMP%/rava-object-leaf-77d3d12cc7174c6f9e9cf187df68f475`.

## Full release boot

Frozen executable SHA256:
`92998E6EF19F8C6EB9817E444BEE8A8AFDFB2CC17AE26C44B60A596FD4254643`.
Server preparation: 30457 classes, 292528 compiled, 56525 unsupported. All 36
then-existing semantic probes pass. The subsequently added AotWorkerGcSem also
passes in a separately prepared, same-executable probe.

Uninstrumented JIT=0 boot reached Done in **150.111 seconds wall**, CPU 147.28125
seconds, peak RSS 924037120 bytes. Fresh pristine-world copy, port 25599, no
snapshot, no concurrent owned compiler/test process. System idle counters were
unavailable, so machine quietness was not certified. The guest's Done (0.797s)
does not include process startup and must not be reported as the wall result.
Evidence: `%TEMP%/rava-product-boot-52fe33bc61a84fcfbc303f189fccca51`.
The <5-second goal is not met. A matching JIT=0 baseline run is required before
attributing a speedup or regression to this set of changes. The earlier RingLane
crash in the packed predecessor remains open despite a passing traced rerun.

The matching pre-direct/pre-container JIT=0 release completed in 168.190 seconds
wall, CPU 129.296875 seconds, peak RSS 965566464 bytes. Evidence:
`%TEMP%/rava-product-boot-e5786cbb3cbc40ecba8ea05e363eba3b`.
Execution environment entries match except the prepared-directory path. Both
runs are uninstrumented and had no other owned heavy processes; idle counters
were unavailable. This single pair shows 10.7% lower wall but 13.9% greater CPU
for the new build, not a general CPU-performance improvement. I/O and scheduling
can affect wall independently; the pair cannot isolate individual changes.

Next investigation: direct prepared dispatch precedes the existing flatcall
route, which already executes trivial getters/constants without an interpreter
frame. Check whether prepared execution is replacing a cheaper structural route
before adding more compiled-body coverage. Do not use method names or observed
receiver history to choose that route.
