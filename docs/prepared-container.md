# Prepared object container

The diagnostic profile contains substantial NtCreateFile self time. Prepared
methods currently open one file per first-linked method, including missing
objects. Replace new offline builds with one indexed container and retain the
loose-file reader for existing layouts. Compiler identity validation stays in
front of both readers. This is compiled code only, never application state.

Keep the index in memory, not all machine-code bodies. Read a requested byte
range through one shared open file. Serialize seek/read with a mutex; release it
before decoding or executing anything, so recursive guest calls cannot deadlock.
Validate the container version and every index range before exposing objects.

Validation: archive roundtrip, missing keys, duplicate keys, truncation and range
corruption; existing prepared build/identity/immutability probe and direct-call
GC probe. Full boot timing remains required to establish any performance gain.

Implemented the versioned container with a sorted index and lazy body reads.
The completion marker is written only after the container has been flushed.
The existing compiler identity check runs before opening the container. Missing
methods return NotFound from the index without opening another filesystem path.
Loose artifacts remain readable when the container is absent.

56 AOT-related unit tests pass. Release probes pass:
- `%TEMP%/rava-prepared-bbe3767269c7449b82874594d1d9d440`: directory/JAR
  preparation, actual first-call AOT entry, fresh Java initialization, unchanged
  artifacts across two runs, incomplete and incompatible build rejection.
- `%TEMP%/rava-aot-dispatch-4fd6315e4b7f45e2a8ab2b9c73db03ab`: exact receiver
  dispatch, static initialization, null/synchronized fallback and moving GC.

These probes validate the storage and call paths, not the full wall-time goal.

## Full classpath validation

Release SHA256: `DD4E97E9F532D23277AF3704E25A28EB388279CC1DB0F88CCE92008F872A10CB`.
Frozen run directory:
`%TEMP%/rava-aot-archive-server-85824c3ddccf427791349639987ac4bd`.
Preparation completed: 30457 classes, 292522 compiled, 56525 unsupported;
`objects.pack` is 298817533 bytes. All 36 semantic probes match Java with JIT=0.
Product validation is still running. No full boot wall-time result exists yet.

The gate fixture audit found that newly compiled ReloadBench.class differs from
the version in D:/code/rava/out. ChunkFill, RingScan and RingLaneInstance match
byte for byte. Do not infer that the ReloadBench timeouts establish a compiler
regression: the prepared set did not contain its current class version.
This mismatch does not establish the cause of the timeouts either.

`objects-current-gates` was independently prepared with current gate classes
first on the classpath, using the same executable: 30457 classes, 292524 compiled,
56525 unsupported. `classpath-current-gates.txt` records that input.
`reload-current-gates.ps1` runs only the missing current-fixture gate, without
overwriting the earlier validation outputs. `boot-nojit.ps1` preserves the fresh
world, benchmark port and JIT=0 setup. Run both separately from other heavy tests.

The static-only build finished with five product passes and a ReloadBench timeout.
The intermediate dispatch build passed all 36 semantic probes; its product run
was cancelled during ChunkFill in favor of testing this newer build. Its
validation-cancelled.json records the owned PID and reason; the resulting
exit/hash error is cancellation, not evidence of a semantic regression.
