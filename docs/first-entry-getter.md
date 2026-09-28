# First-entry getter compilation experiment

2026-09-06, baseline `f7611c6c` including the ThreadLocal storage fix.
The candidate requested compilation before flatcall for exact five-byte instance
getter bodies (aload_0/getfield/typed return), under first-entry JIT and only
when the method had no JIT cache entry. Existing force_compile and its refusal
rules were reused; NativeArgsGuard rooted the arguments during compilation.
No receiver-history selection was introduced, but full no-adapt remained unmet.

Motivation: a getter served by flatcall skips jit_run, potentially leaving a
non-inlined compiled caller on its generic fallback. The experiment tested whether
requesting the compiled entry there pays off on startup.

Evidence: `%TEMP%/rava-first-getter-5e2b27c2c7ad459e92e1f4230fa41b41`.
Frozen binaries, candidate.patch, build/semantic logs and six boot logs are saved.
GetterOrderSem and ThreadLocalRemoveSem matched Java -Xint without exclusions.
No new tests were added. No per-method execution census was collected, so these
results do not quantify the fraction of calls shifted to compiled getters.

Control SHA256: `05ADCBE8983DE95EED4E0275E336AAE44B92C4E0C4070D4807730AA11D1CDB6C`.
Candidate SHA256: `4A61CF6DAE373885565F9A7EE812C26935CA1D4EBC6AC0ED718A374760BA3119`.

| Run | Wall seconds | CPU seconds |
| --- | ---: | ---: |
| null0 control | 27.665 | 45.812500 |
| null1 control | 26.596 | 45.031250 |
| candidate0 | 26.951 | 44.984375 |
| control0 | 26.417 | 43.656250 |
| control1 | 26.993 | 45.296875 |
| candidate1 | 27.255 | 46.109375 |

Fresh worlds, normal first-entry configuration/cap16384, instrumentation off.
Every run reached Done; actual hashes and nonoverlapping launch/Done windows were
verified. Quiet counters were unavailable. Null spread 1.069 s; paired candidate
minus control +0.534 and +0.262 s. No speedup. Runtime edits were reverted and
shared release restored to the ThreadLocal-fixed control. The control's earlier
ten-second survival check is documented in threadlocal-jit-remove.md; these timing
runs themselves did not repeat that post-Done observation.

The existing PC profile was also split into five-second windows without rerunning
the product. Raw analysis is time-slices.py/time-slices.json in
`%TEMP%/rava-execution-profile-319b5ff77f554e23b085eef0182c1e82`.
No window exposes one dominant leaf: boundary work spans startup, while NtCreateFile
is more prominent after 15 seconds. These are sampled locations from the older
pre-ThreadLocal-fix binary, not removable-cost estimates or a new speed result.
