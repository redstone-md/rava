# The profile is flat by function and structured by module

2026-08-28.

## The claim it corrects

Five sessions of profiling this engine have ended at the same sentence: *the host profile is flat
— no function above 3.4%, the top forty sum to under half, so the cost is a long tail and the
only cure is a rewrite.* Every one of those readings ranked FUNCTIONS.

Nobody had aggregated the same samples by MODULE.

## The aggregation

One product boot, `release-diag`, first host table (16,497 samples over 120 distinct symbols),
grouped by the crate path segment:

```
rava::vmstate            25.79%     4254
rava::heap               12.72%     2099
rava::opcodes            11.64%     1921
other/os                  8.76%     1445
mimalloc                  8.24%     1359
rust-std / cranelift      7.40%     1221
rava::tlcache             4.27%      705
rava::jit                 3.56%      587
rava::nextgen             2.76%      455
rava::natives             2.63%      434
ntdll / kernel            2.36%      389
rava::frames              1.94%      320
rava::class_loader        1.61%      266
rava::classfile           1.36%      225
rava::map                 1.31%      216
rava::mem_meter           0.98%      161
rava::owner_bits          0.79%      131
rava::rava_sampler        0.79%      131
```

**`vmstate` alone is a quarter of the host.** That is the module holding `jit_call_body`,
`jit_enter`, `jit_direct_call`, `fu_fastpath`, `jit_fu_invoke`, `invoke_framed` — the machinery
that gets from one guest method to the next.

## Grouped by what the code is for

```
call and dispatch    36.4%   vmstate 25.8 + jit 3.6 + tlcache 4.3 + frames 1.9 + owner_bits 0.8
memory               24.7%   heap 12.7 + mimalloc 8.2 + nextgen 2.8 + mem_meter 1.0
interpretation       11.6%   opcodes
class + parsing       3.0%   class_loader 1.6 + classfile 1.4
collections/natives   3.9%   natives 2.6 + map 1.3
os / runtime         11.1%   other-os 8.8 + ntdll 2.4
codegen               7.4%   cranelift + rust-std
```

## Why this matters after a day of counting populations

The day's population counts ruled out, one by one, every candidate that had a name: crossings
(1.4 s), key derivation (2.4 s), interpreted opcodes (1.1 s), object births (0.5 s), `putstatic`
(1 ms), collection births (absent). Named total ≈ 5.5 s of a 36 s boot, and the conclusion was
"a long tail, therefore architecture".

The module view says the tail is not shapeless. **Over a third of the host is call and dispatch**,
which is exactly what the crossing count could not see: a crossing is one EVENT, but the
machinery around it — resolving, planning, framing, caching, the tlcache, the owner bits — runs
on paths that are not crossings and were never counted as any population.

⚠One boot, one table, symbols from a `release-diag` build (`/OPT:NOICF`, so attribution is
trustworthy — on a plain release build identical functions fold and this grouping would be
fiction). It is a direction for the next session's first measurement, not a result.
