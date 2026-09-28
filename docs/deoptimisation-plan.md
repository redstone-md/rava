# Deoptimisation: the feature that unblocks speculation

2026-08-29. Written after measuring twice that speculation without it costs more than it saves.

## Why, in numbers

Two independent attempts to remove call overhead by inlining, both correct (4 bit-exact gates,
21 differential probes) and both a regression:

```text
four inline arms      10.9 M calls removed   +0.81%
guarded devirt         7.0 M calls inlined   +2.42%
```

**The more we inline, the worse it gets.** That is not the signature of cheap calls; it is damaged
code structure. Our guard keeps BOTH paths live:

```text
if class matches { inlined body } else { the full original dispatch }   -> merge
```

Every speculation splits a straight-line block into a diamond with a merge, extra block
parameters for every live reference, and two live ranges where there was one. Register allocation
and loop-invariant motion get worse across the whole method — including the parts that had
nothing to do with the call.

HotSpot emits the other shape:

```text
if class does not match -> uncommon trap -> deoptimise to the interpreter
<inlined body, straight line>
```

The compiled code holds ONE path. No fallback dispatch, no merge, no extra live ranges. That is
why its guarded inlining pays and ours does not.

## What this engine already has

Unusually much:

* **A resumable interpreter.** `opcodes::run(vm, code, frame)` continues from `frame.pc`; the
  bytecode loop reads it every iteration and exception handling already rewrites it.
* **A flat frame.** `Frame { pc, code, buf, nlocals, class }`, where `buf[0..nlocals]` are the
  locals and `buf[nlocals..]` is the operand stack. One buffer, exactly the shape a deopt must
  materialise.
* **Spill machinery.** `spill_for_gc(b, vars, stack, locals_ptr)` already writes the JIT's
  locals and operand stack into memory at a safepoint — the same operation deopt needs.
* **An exception channel.** A compiled function signals a throw with `vm.jit_pending_set(v)` and
  returns 0; the helper can use it unchanged.
* **A callback ABI.** `extern "C" fn(i64, i64, i64, *const i64, i64) -> i64` with
  `vm_ref_worker_sanctioned()` for the VM, used by every existing helper.

## What is missing: the type map

To rebuild an interpreter frame the runtime must know what each cell IS — a JIT cell is a raw
`i64` and a `Value` is typed. The compiler knows (`JT` per local variable, `SV.t` per stack slot)
and currently records nothing.

So each deopt site needs a **frame-state descriptor**, emitted as a static blob and addressed by
a constant:

```text
struct DeoptSite {
    pc:       u32,     // bytecode index to resume AT (the invoke itself, so it re-executes)
    nlocals:  u16,
    nstack:   u16,
    tags:     [u8],    // JT per local, then JT per stack slot
}
```

## The emitted shape

```text
guard:      class == K ? inline : deopt
deopt:      spill locals + operand stack into the frame buffer
            r = rava_deopt(class_ptr, code_key, &DeoptSite, buf)
            return r                      <- leaves the function; no merge, no fallback
inline:     <body, straight line>
```

`rava_deopt` rebuilds a `Frame` from the buffer and the tags, sets `pc`, calls `opcodes::run`,
and returns the packed result — or sets `jit_pending` and returns 0 on a throw.

## Order of work, each step verifiable on its own

1. **The descriptor and the helper**, exercised from ONE hand-written site with a guard that is
   forced to fail. Gates and probes must stay green with every call deopting.
2. **Per-arm counters from the start** (`deopt_taken`, `deopt_pc_ok`) — three dead mechanisms
   this session hid behind green gates, and none of them had counters.
3. **Switch one existing guarded inline** from the diamond to the trap. Compare: the same
   population, two shapes, one A/B.
4. Only if step 3 pays, convert the rest and re-enable `RAVA_DEVIRT_REF`.

## What could make it not pay

* **Methods with exception handlers already refuse to compile** (`jit_run` bails on
  `!code.exceptions.is_empty()`), so a deopt cannot land in the middle of a try block — one whole
  class of difficulty is absent here.
* Monitors: a `synchronized` region entered in compiled code and exited in the interpreter would
  need the monitor stack rebuilt. Check whether compiled bodies can hold monitors at all.
* GC: the spilled buffer must be a root for the duration of the transition. `frames::push` and
  `cells_push` already exist for exactly this.

## The honest expectation

This does not close a 5x gap by itself. It removes the reason every speculative optimisation so
far has cost more than it saved, which is a precondition for all of them: devirtualisation,
collection inlining, bounds-check elimination, type-based folding. **It is the enabling feature,
measured as such rather than assumed.**
