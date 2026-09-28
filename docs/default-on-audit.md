# Which default-on mechanisms still earn their place

2026-08-28. Ten of the engine's 103 `RAVA_NO_*` kill switches, turned off one at a time against a
boot baseline of **wall 33.4-34.0 / `Done` 0.406-0.420**.

## Why ask

Every one of these was shipped because it measured a win. Nobody re-checks them, and this engine
has already produced one blocker that evaporated in nine weeks
(`docs/arena-maps-are-at-parity.md`). The mirror question is the one nobody asks: **a default-on
optimisation whose benefit has evaporated is now pure cost** — a branch, a table, a cache line,
and code that has to be kept correct.

## Result

| mechanism (turned OFF) | wall | `Done` | verdict |
|---|---|---|---|
| `RAVA_NO_DISPATCH_TIER` | **39.0** | 0.433 | **pays heavily** — +15% without it |
| `RAVA_NO_ENTRYVIEW_CACHE` | 35.2 | 0.433 | pays |
| `RAVA_NO_DT_LEARN` | 34.8 | 0.430 | pays |
| `RAVA_NO_ARGBUF_RECYCLE` | 34.5 | 0.428 | pays a little |
| `RAVA_NO_ALLOC_ROUTE_MEMO` | 34.3 | 0.442 | pays a little |
| `RAVA_NO_BULK_READ` | 34.1 | 0.432 | ~neutral on the boot |
| `RAVA_NO_ARENA_KEYMEMO` | 33.9 | 0.426 | ~neutral on the boot |
| `RAVA_NO_EQ_CHEAP_KIND` | 33.7 | 0.417 | ~neutral on the boot |
| `RAVA_NO_CALL_PLAN` | 34.0 | 0.416 | boot-neutral, see below |
| `RAVA_NO_DTOA_MEMO` | 33.2 | 0.432 | **the single run lied** — see below |

## Two that needed a real A/B, and both changed the answer

### `RAVA_NO_DTOA_MEMO` — the single run said "faster without it". It is not.

A one-off boot at 33.2 s read as the best wall of the session, exactly the shape of the recorded
lesson *"a cache in front of an already-short path is a tax"*. Interleaved, six runs:

```
memo ON    wall 33.8 34.3 34.3   Done 0.412 0.407 0.410
memo OFF   wall 33.8 33.8 34.8   Done 0.415 0.419 0.422
```

Wall ties; **`Done` is cleanly better WITH the memo, every run.** It pays ~2%. The 33.2 was
noise, and deleting on the strength of it would have removed live machinery.

### `RAVA_NO_CALL_PLAN` — boot-neutral, and that is not grounds to remove it

```
plan ON    wall 34.4 34.8 34.3
plan OFF   wall 34.3 33.8 34.3
```

Indistinguishable on the boot, against a recorded **−27.6% disjoint**. But that number was taken
on a call microbenchmark, not on the boot — and this tree's own standing rule is that a
percentage in a microbenchmark is not a percentage of the boot. **The rule runs both ways:
boot-neutral is not microbenchmark-neutral.** Left in place; what it needs is a re-measure on the
benchmark that justified it, not a verdict from this one.

## The opt-in half: eleven mechanisms that are OFF, turned on

Same baseline, one boot each.

| mechanism (turned ON) | wall | `Done` | verdict |
|---|---|---|---|
| `RAVA_ARENA_MAPS` | 34.8 | 0.431 | **parity** — and that reopened the rewrite (own document) |
| `RAVA_ARENA_LAMBDA` | 33.4 | 0.427 | neutral |
| `RAVA_ARENA_NATIVE_NEW` | 33.4 | 0.419 | neutral |
| `RAVA_ARRAY_TRIM` | 33.6 | 0.426 | neutral |
| `RAVA_ALIST_GET_INLINE` | 33.6 | 0.430 | neutral |
| `RAVA_ARENA_ENUM` | 34.0 | 0.410 | neutral |
| `RAVA_ARENA_FIELDLESS` | 34.1 | 0.439 | neutral |
| `RAVA_ARENA_KEYMEMO_ALL` | 34.1 | 0.450 | costs |
| `RAVA_ANEWARRAY_SCRATCH` | 34.3 | 0.445 | costs |
| `RAVA_ARENA_RECORD` | 34.5 | 0.443 | costs |
| `RAVA_OFFLOAD_FIRST` | **37.8** | 0.412 | **now a clear regression** |
| `RAVA_ARENA_SYNTH` | **200.0** | NONE | **hangs the boot** |

Two of those are findings in their own right.

**`RAVA_OFFLOAD_FIRST` is now harmful.** It is recorded in this tree as "the best product number
of the session — wall 53.9 against 112.7". The boot it was measured against no longer exists; at
33.4-34.0 it costs 11%. That claim is closed.

**`RAVA_ARENA_SYNTH=1` does not boot** — 200 s, `Done` never reached. Third flag this session that
fails on its own (`RAVA_JIT_THRESHOLD=4` and `RAVA_NO_ARENA_INLINE=1` are the others).

⭐**And the arena-birth row is a structural answer, not a list of nulls.** Every mechanism that
moves another kind of object off the handle heap — maps, lambdas, enums, records, fieldless
objects, native `new` — is already available and none of them makes the boot faster. Together
with arena maps measuring at parity, that says the arena representation is **not faster per se**:
the win can only come from DELETING the handle heap, and every step toward it is free but not
profitable. The migration has no intermediate payoff. It is all or nothing, and that is worth
knowing before spending a session on step two of five.

## What the audit is worth

It cost about twelve boots. It confirmed the four largest mechanisms still earn (dispatch
tiering alone is worth 15%), it flagged three as boot-neutral for a future look, and it stopped
one deletion that would have cost 2% of `Done`.

⭐The discipline it encodes: **a single boot is a direction, never a measurement** — and the flags
that measure "neutral" are the interesting ones precisely because they are the cheapest things
left to delete.
