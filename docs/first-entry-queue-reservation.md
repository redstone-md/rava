# First-entry queue reservation control

Current working configuration is first-entry JIT with explicit cap 16384,
standard register allocation, standing memory limits, and no prepared archive
or snapshot. The goal remains full wall below five seconds and no adaptive
eligibility. Previous first-entry cap 4096 results are not this configuration.

`RAVA_BG_JIT_EARLY_RESERVE=1` enables an existing queue permit before plan
construction. A full queue then avoids constructing plans only to reject the
finished job at `try_send`. Permits refund on drop or worker receive. The
current default is four compiler threads and queue capacity 16 (the maximum of
the requested depth and four slots per worker). The flag does not change these
values or introduce name/hotness-based eligibility.

This control tests the larger first-entry population; it does not assume prior
ordinary-JIT queue observations apply. It remains opt-in until measured and
semantically verified in this configuration. No new runtime code was necessary.

Same release executable SHA-256:
`078DE86F2C7E04D93B50EB1AE0EA980494602631B30DC1795F7878CDF9452D20`.
Uninstrumented, sequential enabled/disabled/enabled controls all reached full
Done with fresh fixtures:

| Early reserve | Wall | CPU | Product-boot suffix |
| --- | ---: | ---: | --- |
| enabled | 24.763 s | 41.1875 s | 3c29e73257d849f7a5d9724ab137deae |
| disabled | 25.798 s | 46.25 s | 627940d1b1df44b388b587d67839ad14 |
| enabled | 24.521 s | 40.828125 s | 9a178c13b13748a18801f56422b491e4 |

Earlier adjacent standard controls were 25.555, 25.680 and 26.020 seconds.
This repeat supports a useful candidate in the high-cap first-entry mode, not
an exact universal percentage. Quiet counters were unavailable. Lower CPU is
consistent with avoided planning, but plan counts/time were not directly
instrumented in these runs. Do not claim all CPU reduction as planning time.

Artifacts and the running semantic validation log are in the directory named by
`%TEMP%/rava-first-entry-reserve-current.txt`. The full 41-probe validation uses
explicit first-entry, cap 16384 and early-reserve flags in `sem-env.json`.
Validation is pending until the terminal suite result is inspected. No runtime
default or source fixture changed. The under-five-second goal remains unmet.

Validation completed: all 41 differential probes match Java -Xint with the
suite's existing declared divergences. Terminal session exit was zero.
`sem.log` and `sem-env.json` preserve the exact run. This replaces the pending
status above. Full product hash/reload validation remains separate; do not
promote a global default on the basis of semantic probes and boot timing alone.
Early reserve is retained as an explicit flag in subsequent first-entry
experiments, with cap 16384 and standard register allocation.

Integration follow-up: early reservation now defaults on only inside explicit
first-entry mode; the environment override remains authoritative. See
`first-entry-product-validation.md` for five passing product hashes, the
certified reload pair, and the new build's 25.837-second full boot. The earlier
opt-in-only status is superseded by this integration. Global no-adapt compliance
and the below-five-second objective are still unproven.
