# Round 3 — what landed, and why two defaults were reverted

Five tracks built; every Measure agent and the completeness critic died on an access error, so
nothing in this round carries a wall verdict yet. The mechanisms below are real and their COUNTS
are measured; the wall numbers are owed.

| flag | default | mechanism | counted evidence |
|---|---|---|---|
| `RAVA_FU_FIELD_IDS` | ✅**ON** | `fu_double_view`/`fu_int_view` re-resolved the fastutil list's `a` and `size` field ids BY NAME on every fast-path call | the chain `resolve_field_id < fu_double_view < jit_fu_invoke < rava_fu_invoke` is **14.0% of the ring**, and the only chain under `resolve_field_id` in the window (396/396 samples) |
| `RAVA_DISPATCH_GATE` | OFF | per-owner bit plan replaces `dispatch_owner`'s linear cascade of five sub-dispatchers plus ~40 owner tests, run once per super-chain entry | sub-dispatcher entries **−54.7%**, 81.8M removed on ChunkFill |
| `RAVA_LAMBDA_SPLICE` | OFF | `Vm::apply` cloned the whole `Lambda` to answer a bool; `invoke_lambda` cloned the captures again and grew the clone | block C argvec allocations **−61.8%** |
| `RAVA_TLS_CLUSTER` | OFF | one inlined TLS cursor instead of a `LocalKey::with` per access | `LocalKey::with` is 5.5-8.9% of RIP self-time in EVERY window |
| `RAVA_ARGBUF_RECYCLE` | OFF | the three NATIVE dispatch arms borrowed the pooled arg buffer and dropped it, draining the pool | pool imbalance, found by census (it refuted the brief's own Vec-growth hypothesis) |

## Two defaults were reverted from ON to OFF

The dispatch-plan and lambda-splice tracks each shipped their flag **default-ON** on a structural
argument — "it only removes work"; "the argument vector is element-for-element identical in both
arms" — while stating outright that **no wall A/B was run**, walls being forbidden in their phase.

Both arguments are sound and both counts are real. They are still not defaults. This project's
standard is that only interleaved separation ships a default, and the standard exists because
plausible-but-unmeasured changes have failed here repeatedly: a `charAt` intrinsic measured **9.6×**
on its microbench and moved the boot by **exactly zero**; the scratch buffers' halved mallocs priced
out at ms-scale against a 50s window. A count of removed work is a necessary condition for a win,
never a sufficient one — that is the single most expensive lesson of this whole arc.

## Also settled this round

`RAVA_DISPATCH_FASTFRAME` is **retired as a flag** and its code kept unconditional: it and
`RAVA_TLS_CLUSTER` were proven substitutes rather than addends (the duplicate-lever incident, twice
in three rounds — the second time colliding through a shared *module* rather than a shared file, so
the file-ownership rule did not catch it). The cluster's OFF arm was also **self-handicapping**: a
null cursor doubled as the "gate unresolved" state, so the disabled arm re-entered a cold,
never-inlined path per access — its round-2 A/B was biased against itself and is only now meaningful.

## Measured since

**`RAVA_FU_FIELD_IDS` → DEFAULT-ON.** Six interleaved full boots on the Done window, P-cores,
quiet machine, fresh binary:

| arm | `Done (s)` |
|---|---|
| OFF | 49.272 / 47.225 / 47.183 |
| ON | **45.838 / 42.885 / 42.947** |

**Every ON below every OFF, min-of-N −9.11%.** The profile predicted 14.0% of the ring for the
whole `rava_fu_invoke` chain and 6.7% for `resolve_field_id` inside it, so a fix landing at 9.1%
of the Done window is where it should be — mechanism and magnitude agree.

⚠ **The first attempt at this A/B had to be thrown away**: `boot_flag_ab.ps1`'s default exe path is
`target
elease
ava.exe` while every build that day went to `target\probe`, so it measured a
23-hour-stale binary and reported Done 112.264s against a real ~47s. `bench/assert_fresh_exe.ps1`
now refuses that run. Third measurement trap of this kind in one session.

## ⛔ The other four mechanisms: REFUTED as wall levers, together

Measured as ONE combined arm — the cheap question first — six interleaved boots, P-cores, quiet
machine, fresh binary:

| metric | OFF | ON | min-of-N | separation |
|---|---|---|---|---|
| wall | 108.3 / 103.9 / 104.4 | 106.3 / 100.8 / 104.5 | **−2.98%** | overlap |
| A+B | 38 / 36 / 35 | 37 / 35 / 37 | 0% | overlap |
| block C | 21 / 20 / 20 | 20 / 19 / 19 | −5% | overlap (±1s quantisation IS ±5% here) |
| Done | 48.489 / 46.924 / 47.683 | 47.836 / 45.924 / 46.974 | −2.13% | overlap |

`RAVA_DISPATCH_GATE` + `RAVA_LAMBDA_SPLICE` + `RAVA_ARGBUF_RECYCLE` + `RAVA_TLS_CLUSTER` are worth
**at most ~3% of wall between them, and nothing separates.** All four stay default-OFF.

The direction is consistently favourable — ON wins the minimum on wall, C and Done — so this is a
ceiling, not a sign flip. But the counts behind these four were enormous: 81.8M sub-dispatcher
entries removed, argvec allocations −61.8%, a pool that was draining 43% of the time, and
`LocalKey::with` at 5.5-8.9% of samples in every window. **They bought ≤3%.**

That is the charAt lesson a fourth time, and now with a stronger form: it is not merely that a
count can overstate a prize. **The cheap host-bookkeeping seam is largely mined out.** Six such
fixes shipped earlier today for real, separated wins; these four did not, and they were chosen
from the same profile by the same reasoning. What remains in that class is diffuse.

Splitting the arm was deliberately skipped: four separate A/Bs cost 24 more boots to apportion a
delta that does not separate in aggregate.

## Owed

The dispatch plan and the lambda splice on block C, the TLS cluster on both windows, and the
argbuf recycle. Plus the two verdicts round 2 left open — `RAVA_VIRT_CLASS_MEMO` and
`RAVA_SORT_KEY_MEMO` were both judged on the A+B window while both target block C; a two-marker
block-C harness is still not committed, and three rounds have now wanted one.

Also open: the DEFAULT path's own baseline moved between rounds (round-2 tree read Done 65.585s on
a single run; this round's OFF arm reads 47.2s across three), and only one unflagged behavioural
change landed by default in round 3 — the fast frame going unconditional, which round 2 had
measured at −6.32% WITH OVERLAP. An attribution A/B of the two trees is running to settle whether
the gap is that change or single-run spread.
