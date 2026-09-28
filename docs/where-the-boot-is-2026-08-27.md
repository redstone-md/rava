# Where the boot is, measured — 2026-08-27

Host profile of a product boot, `release-diag`, **87.9% of wall is in the Rust host** and 11.8%
of samples are inside compiled code. By function:

```
 4.4%  mi_malloc_aligned          0.9%  mi_free              ⇒ allocator      5.3%
 3.5%  memchr                                                ⇒ regex          3.5%
 3.2%  Heap::gc                   0.8%  retire_object_slot   ⇒ legacy GC      4.0%
 3.0%  invokevirtual
 2.8%  jit_call_body   1.3 jit_fu_invoke  1.3 jit_direct_probe  1.2 jit_indy
 1.1%  jit_direct_call 1.0 dispatch_miss  1.0 jit_call  0.8 jit_direct_invoke
 0.8%  jit_enter                                             ⇒ JIT seam      11.3%
 1.0%  Heap::get_field 0.7 get_field_hint 0.6 field_id       ⇒ field access    2.3%
 1.3%  _NLG_Return2 (MSVC unwind helper)
```

Top single function **4.4%**. The tail is 120 rows.

## Axes closed by measurement today

| axis | population | measurement | verdict |
|---|---|---|---|
| interpretation | 197.1M opcodes | remove 70% of them | boot unmoved (0.75 s of 48) |
| allocation *count* | 113.5M | 462 sites, top 6.5%; 1038 paths, top 3.6% | flat, ~0.26%/path |
| allocation *tax* | 223.8M atomic RMW | batched per-thread | **-4.4% boot / +3.06% for the old code on ChunkFill** |
| allocator page policy | 110M frees | `MIMALLOC_PURGE_DELAY` 0 vs 500 | -0.01%, and no RSS cost either way |
| GC cadence | — | `RAVA_GC_REARM_TIGHT=0` | -0.45% (floor 1.2%), **RSS +275 MB** ⇒ current default is right |
| GIL | — | census | **3 acquires per boot** — not a per-event tax at all |
| compiled ref-store barrier | — | read the emission gate | never emitted in production (all 4 gates diagnostic) |
| offload memo | 343k queries | probe | 96.6% hit, 11.7k analyses — healthy |
| map-key memo | 750k renders | `RAVA_KEYRENDER_STAT` | **hit=0, rejected=749,996** — dead, but the population is only ~1M ⇒ 0.5% |
| compiler refusals | — | `RAVA_DBG_CBAIL` | **32 bails per boot** (31 `exceptions`, 1 `emit`) — the driver almost never refuses |

## What is left, and why it is one thing

The seam is the biggest coherent cluster (11.3%) and it does not shrink by compiling more:
59.4% of crossings go to `java/*` methods that have **no bytecode**, and lowering the tiering
threshold to widen coverage made the boot 3-8x worse (partial coverage means nearly every call
crosses). Removing the seam requires `java.util` as guest bytecode; that was measured at +8.46%
because it moves births onto the handle heap; the handle heap is what the RSS decomposition
prices at **4 bytes of metadata per byte of object data**.

⇒ seam → java.util as bytecode → collections in the arena → retire the handle heap. Each link is
now a number rather than an opinion, and they are the same line.

## What made the difference today

Not a hot site — a **per-event tax**. The one thing that paid was work every allocation did
regardless of what it was for. When a profile is flat at every scale, that is the shape to look
for: not "which function is hottest" but "what does every event in the largest population pay".
