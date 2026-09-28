# Inline-local reuse: memory motivation and rooting contract

The current lockfile uses Cranelift frontend 0.132.1. Its installed `ssa.rs`
stores definitions as a dense `SecondaryMap<Variable, SecondaryMap<Block, ...>>`.
A definition in a late-numbered block can therefore allocate a large row even
for a fresh variable used only by one splice. Rava's inline emitter declares
`max(max_locals, argument_slots)` fresh I64 variables for each splice. This is a
concrete source-level growth mechanism, not a measurement of total compiler RAM.

Context7's Wasmtime documentation describes `def_var`/`use_var` SSA construction;
the exact stack-map behavior was checked against the installed 0.132.1 source.
`def_var` replaces the variable's definition in the current block. During
`FunctionBuilder::finalize`, stack-map variable markers propagate to the values
still recorded in those definition tables. A saved earlier SSA value may remain
live even after its variable identity is reused.

Three dependency-contract tests reproduce the distinction with a reference-sized
I64 parameter kept live across a call:

- Separate variable identities preserve the stack map.
- Same-block reuse without explicit value rooting loses it.
- Explicit `declare_value_needs_stack_map` on the saved SSA value preserves it.

All three tests pass under `next-gen-gc`. Evidence:
`%TEMP%/rava-inline-local-contract.log`. These tests build and inspect Cranelift
IR; they do not execute a moving collector or establish a defect in current Rava.
No production variable reuse was introduced, and no speed improvement is claimed.

## Required implementation proof

A prospective function-local pool must separate simultaneously active inline
frames, restore allocation depth on all exits, and initialize reused locals
before use on every branch. Retained SSA results must keep their own root markers
when the source variable is redefined. Reference returns, nested splices, joins,
loops and live caller operands across GC all require coverage. Variables must
never escape to a different FunctionBuilder or compiled function.

Reuse is not yet implemented. The first-entry allocation failure documented in
`inline-expansion-budget.md` and the full wall-under-five-seconds/no-adapt goal
remain open. Do not replace the proof with class-name exclusions or a snapshot.

Follow-up: a scoped pool prototype passed its lease and GC probes but did not
resolve the allocation failure and was removed. See `compile-memory-phases.md`:
frontend finalization completes; the next investigation is IR work admitted to
`finish_compile`, not another unmeasured local-pooling attempt.
