# The optimisation we do not have

2026-08-29.

## Two months of levers, and why every one of them measured zero

Dispatch 16%, the seam 14%, the frame 42%, the caches, the allocations, the operand
representation, the lane — each was priced, built or refuted, and each landed on the measurement
floor. They have one thing in common, and it took a decomposition of the whole boot to see it:
**every one of them makes a CALL cheaper.**

HotSpot does not make the call.

`ArrayList.size()` in a JVM is not a call at all. The JIT sees a monomorphic call site in the
profile, devirtualises it, splices the body in behind a class guard, and what remains is a field
load. A cheap call cannot beat an absent call, however finely it is tuned — which is why six
levers in a row measured nothing.

## What our inliner accepts

`collect_inline_targets`, read directly:

```text
invokestatic           -> inlined
invokespecial          -> inlined (statically bound, no guard)
invokevirtual (0xb6)   -> inlined when the method is final-effective, no guard
invokeinterface(0xb9)  -> never
invokedynamic (0xba)   -> never
```

**`List.size()`, `List.get()`, `Iterator.hasNext()`, `Iterator.next()`, `Map.get()` — the whole
collection API as real code writes it — go through interfaces.** So on `bench/StreamApplyCost.java`
the inliner logs (`RAVA_JIT_INLINER=1`) exactly **zero** collapses.

## The profile says the optimisation applies

`RAVA_DIAG=devirt` on a product boot, counting CALLS by how many receiver classes their site has
ever seen:

```text
mono   2,074,791   76.8%
bi       245,913    9.1%
poly     382,611   14.2%
```

**Three quarters of virtual and interface calls are monomorphic**, 86% are mono-or-bi. That is
the shape guarded devirtualisation exists for, and it is the same shape that makes the technique
pay in every production JVM.

## And this is why java.util-as-bytecode kept losing

The migration was measured again today, with the JIT trace confirming what earlier attempts could
not: **the guest classes DO compile** — `ArrayList.size`, `iterator`, `Itr.hasNext`, `Itr.next`,
`add`, `Objects.requireNonNull`, all `[COMPILE]`d. And guest bytecode is still **5-8% slower**
than the native on `StreamApplyCost` (6.53/7.21/6.63 s against 5.50/6.45/5.77, 3 of 3), and on
the boot it turns 1.29 M `fu_fastpath` refusals into 7.9 M.

The old record has it winning 5.0x and 12.2x. Both of those were measured against the SLOW native
path, before the collection intrinsics existed. Today a native served by `fu_fastpath` costs
65 ns and compiled guest bytecode costs about the same, because **both make a call**.

⇒ The bytecode-versus-native argument is not the question and never was. The question is which
side can be INLINED, and only one of them can: **a Rust native can never be spliced into a
compiled caller; guest bytecode can.**

## The order this puts the work in

1. **Guarded devirtualisation and inlining of monomorphic virtual/interface sites.** The
   ingredients are all here — the per-site receiver profile (`tlcache::obj_inline`/`ref_inline`,
   the machine-code PIC, the freeze table), an inliner with slot remapping and depth-8 splicing,
   and class-guard emission already used elsewhere. What is missing is one question at compile
   time — *is this site monomorphic?* — and one emission: `guard(class_id == K) { body } else
   { call }`.
2. **Then** `java.util` as guest bytecode becomes strictly better than the natives, because only
   then can `size()` collapse to a field load. Today it is strictly worse, and the measurement
   above says why.
3. **Then** the seam and the fast path shrink together, because both exist to service calls that
   would no longer be made: 38.4 M `fu_fastpath` (8.12%), 21.3 M seam prologues, 8.7 M general
   paths.

⚠The census counts calls that reach the INTERPRETER's per-site cache — 2.7 M of them. Compiled
code uses its own machine-code PIC and never touches this cache, so the 76.8% is a shape reading,
not a population. Sizing the compiled half is the next measurement, and it decides how much of
the 8.12% is reachable.
