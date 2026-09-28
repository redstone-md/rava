# RingLane failure and focused GC checks

The packed release DD4E97E9F532D23277AF3704E25A28EB388279CC1DB0F88CCE92008F872A10CB
passed all 36 semantic probes, ChunkFill and RingScan, then failed RingLane.
Evidence: `%TEMP%/rava-aot-archive-server-85824c3ddccf427791349639987ac4bd/validation`.
The stderr reports worker=true and a zero-header object at 0x200ea25f8 being
used as RandomSource.nextInt's receiver, then STATUS_ACCESS_VIOLATION.
RingLane.class matches D:/code/rava/out byte for byte. The ReloadBench fixture
mismatch therefore does not explain this failure. No cause has been established.

The same executable, current-gates prepared set and RingLane inputs were rerun
with RAVA_TRACE_ARENA=1. It exited zero and produced HASH=37539c6c375d0c3,
LANE_MS=148404. Evidence: `validation-ring-repro` under the same root.
The ad-hoc runner then incorrectly applied ReloadBench's warm-output check to
RingLane, after the exit/hash checks had passed. Preserve ring-repro-original.ps1
as executed; ring-repro.ps1 corrects that runner condition. A successful traced
rerun does not close the untraced crash or establish that it is timing-dependent.

AotWorkerGcSem stresses retained references in CompletableFuture suppliers while
allocating 80000 objects with a 64-KB nursery. The dedicated script requires a
prepared work-method entry, actual GC and output equal to Java (sum=280000).
It passes for both frozen builds:
- Packed build: `%TEMP%/rava-worker-gc-f4c61f3ecdf240c7b4b0cb08a12cc6fb`.
- Instance-leaf build: `%TEMP%/rava-worker-gc-57974f5082b042d9b6adc644a12832b4`.

The first exploratory version used string literals in assertion paths, making
the work method ineligible for AOT. Removing those literals and requiring the
actual work entry avoids mistaking interpreted fallback for an AOT root test.
This focused probe does not prove it exercises the failing RingLane schedule.
Do not disable worker execution or weaken GC checks on this evidence alone.
