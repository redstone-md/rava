# The hottest host function is 4.3%, and deleting it entirely is worth 2.1%

*2026-08-26*

## The candidate

A fresh host-symbol profile of a product boot (90.9% of samples in the Rust host) finally has a
clear number one, and it is not a dispatcher or an allocator:

```
4.3%  drop_in_place<Option<Value>>
3.3%  mi_malloc_aligned
3.2%  memchr
2.7%  opcodes::invokevirtual
2.6%  Heap::gc
2.2%  jit_direct_probe   2.1% jit_fu_invoke   1.8% dispatch_owner_cascade
```

It is also the top host function in **6 of 10 time buckets** — uniformly hot, not one phase.

`Value` is not `Copy` for exactly one reason: `Value::Str(RStr)` and `RStr(Arc<String>)`. One
variant of eleven owns anything; the file already says so, and already hand-writes `Clone` to
keep the common case out of line. But `Drop` cannot be hand-waved away the same way — while any
variant owns a refcount, `needs_drop::<Value>()` is true and every `Option<Value>` that dies
calls out to glue.

## The probe

`RStr` is fully encapsulated — `as_str`, `ptr_eq`, `Deref`, `AsRef`, `Borrow`, and the `From`
family. **Zero constructions of `RStr(..)` exist outside `value.rs`.** So the representation can
be swapped in one file: `RStr(&'static str)`, `Box::leak` in each `From`.

The leak is deliberate. Dedup would put a lock on every string creation and the probe would then
be measuring an interner instead of a drop; one leak per creation is the same single allocation
`Arc::new` already made, so the two arms differ in the drop and in nothing else.

`ptr_eq` becomes strictly weaker (no dedup ⇒ it answers false where Arc identity answered true),
which is the safe direction: callers fall back to the byte compare they already have.

Result: **`Value` is `Copy`, and the binary is 522 KB smaller** — that is the drop glue and the
clone match, gone.

## The measurement

4 gates bit-exact, 20/20 probes vs `java -Xint`. Then a two-binary interleaved boot A/B
(a repr change cannot be a runtime flag), 3 rounds, arms alternating within each round:

```
r1: BASE cpu=43.2  COPY cpu=42.9
r2: BASE cpu=43.2  COPY cpu=42.0
r3: BASE cpu=42.9  COPY cpu=43.0
CPU-s: COPY min=42.0  BASE min=42.9  delta=-2.10%  disjoint=False
```

**-2.10%, overlapping, at the +-2.5% CPU floor.**

## What it means

Deleting the single hottest function in the profile — completely, not shrinking it — bought
about two percent. And a production form can only be worse than this probe: it must add interning
to stop the leak, and interning is pure addition.

So the operand-representation line, which stood at an estimated ~6.5% of wall across 622 sites,
is **priced end-to-end at <= 2.1% and closed.**

The wider lesson is the one the flat profile has been implying for weeks, now demonstrated
rather than argued: **this profile cannot be attacked row by row.** A 4.3% row is not 4.3% of the
wall — it is 4.3% of the samples of a host that is itself doing work the guest program should not
be paying for at all. Removing the row leaves the work.
