# Prepared call exception boundary

Prepared methods currently resume bytecode after the general invoke and
invokedynamic callbacks even when the callback has set a pending exception.
The runner only propagates that exception after the method returns. This can
execute extra effects or fail to return at all if later bytecode loops.

Add a non-allocating pending-state query and an early native return immediately
after each general call callback. Preserve the exception for the existing
runner to propagate. Inlined numeric/object leaves without these callbacks
do not need this additional query. This uses runtime exception state, never
target names, call history or workload-dependent admission.

Test the emitted code for all five invoke opcodes, both throwing and normal
returns: prior writes remain, following writes happen only on normal return.
Append the callback to the symbol table to preserve old indices, and advance
the full-method cache identity. This does not certify exception handling for
every other callback, nor the unresolved RingLane crash or five-second goal.

The regression first failed on the old translator: a throwing virtual callback
still reached the later return of 9. The final test uses the real Vm pending
slot and production pending callback, checks that the exception remains available
to the caller, and covers virtual, special, static, interface and dynamic calls.
All 64 AOT-selected tests pass, one ignored, including existing moving-GC call
tests. Logs: `%TEMP%/rava-call-fault-before.log` and
`%TEMP%/rava-call-fault-runtime.log`.

The first full test attempt hit a null callback address in an older invoke test
fixture, which resolved every symbol except its stub to zero. That fixture now
uses the normal symbol resolver for the new pending query. The subsequent
serial and default-concurrency suites both pass. Runtime code was not relaxed
to accommodate the fixture. Release/product measurements remain outstanding;
the previous frozen release's Defender detection remains unresolved.
