# Operand-stack values across AOT control-flow edges

Objective remains full process-to-Done below five seconds, no adaptation and no
snapshots. This change addresses structural AOT refusals identified in
`aot-fallback-coverage.md`; it does not certify the whole runtime's admission policy.

The previous emitter reset its operand stack at every block and refused any
nonempty outgoing stack. Short-circuit booleans and conditional reference values
therefore could not compile, including the inspected BlockStateCache lambda and
nontrivial BooleanOp truth functions.

BlockStacks owns incoming stack shapes and a worklist of reachable blocks.
The first incoming edge creates I64 block parameters; subsequent edges must
agree on stack height and reference/wide flags. The emitter visits a block only
after its incoming signature is known, and seals blocks after all edges have
been emitted. Backedges validate the same signature. No invocation history,
receiver observation or class/method-name list chooses eligibility.

A separate function-entry block owns the locals/constant-table ABI parameters.
Bytecode PC 0 can consequently be a loop target with an empty operand stack.
Conditionals and jumps pass operands directly. Switch edges with a nonempty
stack use small bridge blocks because Cranelift Switch does not take block
arguments. Reference flags survive merges, so the existing callback spill/reload
protocol roots and forwards merged references. Unreachable blocks still cause
a refusal, preserving the previous conservative coverage boundary.

Validation so far:

- 47 AOT unit tests pass, including serialized machine execution of boolean,
  reference and wide joins, nonempty-stack backedges, PC 0 backedges, both switch
  encodings and incompatible reference/width merge refusals.
- A merged reference survives a forced moving collection on both branch edges;
  the test asserts a changed address and intact object contents. The shared
  older rooting fixture now registers its thread, so its collection tests also
  prove actual movement instead of merely reading an unmoved address.
- CfgStackSem matches Java -Xint with confirmed prepared method entries: boolean
  truth cases, nullable reference choices, long extremes, switch results and
  selected/unselected null dereferences.
  Evidence: `%TEMP%/rava-cfg-stack-7ae465fd795a43d58579f60dc64c58b7`.
- All 34 ordinary semantic probes match, with existing declared divergences.

Saved release SHA256:
`5FD560D8C8B51CA364AE3DB193594A786D3D84F82505D7A9B8C902C966129615`.
Executable, full preparation and logs:
`%TEMP%/rava-cfg-stack-server-85cd7280ca544440aace57662e7ba035`.
Full preparation completed: 30,450 classes, 292,497 compiled methods and 56,521
refusals, versus 270,351 compiled methods in the int-shift build. Full prepared
semantics, product hash gates and wall measurements are running via the saved
validate.ps1 and isolated boot helpers. Do not infer a startup gain from
additional compiler coverage.
