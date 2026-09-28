# Prepared arithmetic exception exits

The full AOT translator calls the integer divide callback on a zero divisor
and then rejoins ordinary execution. Its long divide callback likewise returns
a placeholder on division by zero, after setting a pending exception. Continuing
bytecode can execute effects or loops before the shared runner propagates it.

Terminate the integer zero-divisor branch immediately after the callback. For
long operations, retain the callback and branch on the original divisor before
executing another bytecode. Preserve overflow and nonzero behavior. This proof
uses the arithmetic operand, not an execution-history or workload rule.

Validate emitted machine code for all four division/remainder opcodes: zero
must invoke the fault callback and leave a following local store untouched;
positive and negative nonzero divisors must execute that store normally.
This is one exception boundary, not a claim that all AOT callback exceptions
or the intermittent RingLane crash have been fixed. No boot gain is claimed.

The new machine-code test failed on the unmodified translator: integer division
by zero returned the later constant 9 rather than exiting with placeholder 0.
After the fix, all four operations preserve a preceding local write, skip the
following write on zero, and execute it on nonzero divisors. The full AOT test
selection passes (63 passed, one ignored). Existing arithmetic edge tests also
pass. Logs: `%TEMP%/rava-arithmetic-fault-before.log` and
`%TEMP%/rava-arithmetic-fault-after.log`. The full-method cache input namespace
is advanced so old code cannot satisfy the new compilation request.

Full server validation and performance measurements remain outstanding. The
prior release binary's Defender detection remains a separate unresolved issue.
