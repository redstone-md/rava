# Receiver-history collection: rejected performance experiment

After structural inline selection stopped consuming receiver observations, an
experiment gated observation writers behind an explicit diagnostic flag. It
passed correctness checks but supplied no performance reason to change defaults.
The gating code and flag have been removed. Structural inline selection remains.

In the archived candidate only, `RAVA_RECEIVER_HISTORY_DIAGNOSTICS=1` enabled
collection and unset disabled it. It was not consumed by target selection.

Compiled direct-entry and SAM producers returned before decoding receivers or
incrementing history counters. Common table writers and the diagnostic-only
lambda-provenance query were also gated. Interpreter call sites still computed
primitive arguments to the disabled writer.

Dispatch caches and deopt handling were unchanged. The experiment was about
collection overhead, not removing additional execution-selection consumers.

Candidate SHA-256:
`B7DDEDB6D2DC2755A9681CB70F3870A33134FF5D80970837BFAD4C40961DFD83`.
Evidence: `%TEMP%/rava-history-diagnostics-20260905`.

Release build, all 28 default semantic probes, five product hashes and certified
cold/warm ReloadBench outputs pass. Diagnostic-mode LambdaSiteSem, CollSem and
CompSem also match Java. Source is archived under the evidence directory's
`source` subdirectory.

Two same-binary pairs, in opposite orders, reached full Minecraft Done:

| Collection | Wall to Done | CPU | Evidence directory suffix |
| --- | ---: | ---: | --- |
| OFF | 33,697 ms | 35.531 s | `d13a5e51782d474383bbe13237ed4a40` |
| ON | 32,355 ms | 34.313 s | `a1f3891e7e0a4de19eab0cb7f6536f7b` |
| ON | 32,929 ms | 35.313 s | `41494027d1ef45a8b18172caa6b5f673` |
| OFF | 33,479 ms | 35.703 s | `21d14c55939b4fae9be1ad3d489181e2` |

Directories: `%TEMP%/rava-product-boot-<suffix>`. Same standing configuration,
pristine fixture and affinity; host quiet counters were unavailable. Both pairs
favoured collection. This small series does not explain the cause or establish
a precise regression, but supplies no verified gain from disabling collection.
The prototype was removed rather than calling fewer source operations a speedup.
