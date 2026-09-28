# The blocker on the handle-heap rewrite is stale

2026-08-28.

## What was recorded

The standing structural verdict on this engine has been: the profile is flat to the bottom, no
micro-optimisation clears the floor, so what is left is a point rewrite — **remove the handle
heap**. And the recorded precondition:

> Step 1 = an arena representation of collections without regression (`RAVA_ARENA_MAPS` is
> currently **+48%** on the boot) — all the risk is there; if it does not work, the rewrite does
> not start.

Nobody had re-measured that number since it was taken.

## What it measures today

Interleaved boot A/B, P-cores, `bench/boot_flag_ab.ps1`:

```
RAVA_ARENA_MAPS=1   wall 34.8 34.8 34.8   Done 0.428 0.433 0.431
default             wall 34.3 34.8 34.8   Done 0.413 0.412 0.478
```

**Wall identical. `Done` overlapping. Parity — against a recorded +48%.**

And it is correct by every check this project owns, with the flag on:

* 4 bit-exact gates: `chunkfill` OK, `ringscan` OK, `ringlane` OK, `ringlanei` OK
* all 21 differential probes match `java -Xint`

## What that unblocks, and what it does not

Step 1 is done. It was done by the accumulated work since that +48% was taken — the stale number
was the only thing still saying the rewrite could not start.

What still holds the handle heap is the **other** synthetic field. Counting the sites that name
each:

```
"$map"     41 sites     <- has an arena backend (crates/rava-interp/src/arena_map/)
"$elems"   81 sites     <- no arena backend
```

So the list side is now the larger half of the migration, and it is where the risk moved.

## Why it stays opt-in

This is a representation migration whose value is unblocking, not a measured win. Turning it on
by default would be risk without gain until the list side lands and the handle heap can actually
go. The flag flips when the migration completes, not before.

## The lesson, which is the same one this session keeps paying for

A number in a comment is a measurement with a date on it, and this engine changes fast enough
that a nine-week-old blocker can simply have evaporated. **Re-measure the thing that closed a
line before accepting that the line is closed** — it cost one A/B here and returned the largest
structural decision in the tree.
