# Flat direct-call cache experiment

The instruction trace in `jit-direct-probe-instructions.md` attributes most
selected direct-probe samples to its nested lookup. Replace that representation
with one map keyed by `(caller, constant-pool index, receiver guard)`.

Preserve ordered collision vectors, arg0 checks, lambda guards, negative-cache
handling, root registration, worker restrictions and publication sites. No new
cache, global state or execution eligibility rule is introduced. The existing
engine diagnostic continues to count distinct call sites instead of receiver
rows. Changes are confined to existing owners; no new source file exceeds 500
lines. Frontend state and animation requirements do not apply.

Validation plan: preserve the current normal release as a control, build the
candidate, run all differential semantic probes, then use fresh fixtures for
sequential null and interleaved control/candidate boot runs. Do not promote this
representation on source-level simplification alone. Full product hash checks
are required if the candidate is retained.

The goal remains wall below five seconds, no adaptive eligibility, no snapshots.
This representation does not certify the remaining runtime's no-adapt compliance.

## Result: rejected

The release build passed, and all 41 differential probes matched Java -Xint
with the suite's declared divergences. Full uninstrumented boot measurements:

| Order | Representation | Wall seconds | CPU seconds |
| --- | --- | ---: | ---: |
| null 0 | nested control | 25.383 | 42.28125 |
| null 1 | nested control | 24.986 | 42.609375 |
| B0 | flat candidate | 25.148 | 42.4375 |
| A0 | nested control | 25.116 | 42.375 |
| A1 | nested control | 25.148 | 42.453125 |
| B1 | flat candidate | 25.300 | 41.765625 |

Both candidate/control paired wall differences are positive (+32 ms, +152 ms),
within the 397 ms null spread. No startup speedup was demonstrated. No claim of
statistical significance or universal regression is made from two pairs.
The runtime patch was removed and the preserved control executable restored.
Product hash gates were not run for this rejected candidate.

Every boot reached Done using a fresh fixture, explicit first-entry JIT and cap
16384, default early queue reservation, standard allocator and standing memory
limits. The six copied executable hashes and recorded environments were checked;
no profiling, prepared archive, checkpoint or restore flags were present.
Performance counters were unavailable, so quiet CPU conditions were uncertified.

Control SHA256:
`858AEA5B921A1E98173681B27F625F1BBB6CB2C95D73C007B56DE37734CF16F4`.
Candidate SHA256:
`0D1EA561E0CFED6DD6B2B060A9393EE5063EE24EE4A11474B2B8095DC4783C25`.
Artifacts, candidate patch, preserved binaries and semantic outputs:
`%TEMP%/rava-flat-direct-1843ae7585c245c38ebca89c73b22652`.

This changes the next investigation: source attribution to a table lookup is
not sufficient grounds to expect a faster boot by changing its representation.
Investigate eliminating crossings through the host call boundary, rather than
another lookup-layout variation. The five-second goal remains unmet.
