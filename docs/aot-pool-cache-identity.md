# Full AOT cache identity across class versions

The full AOT cache formerly keyed an object by class name and instruction bytes.
That distinguishes different class names but not two versions of the same class
whose constant-pool indices stay fixed while their contents change.

## Reproducer and correction

`bench/aot_pool_cache_sem.ps1 -Exe <absolute-executable>` compiles two versions
of `AotPoolVersion`. Their `value()` methods both execute `ldc #7; ireturn`.
Slot 7 holds integer 100000 in version 1 and 200000 in version 2. Each main calls
the method 1000 times; Java outputs 100000000 and 200000000 respectively.

With the previous binary, running version 1 then version 2 against one cache
produced **101500000** for version 2, with exit code 0. The first 15 calls use
the new literal; the remaining calls use the cached code for the old literal
after the test's threshold of 16. This is an observed wrong result, not merely
a suspected key collision. Old evidence:
`%TEMP%/rava-aot-pool-cache-e77af8ecbbff4ecead62731ce776e46e`.

The `full_v2` key adds a SHA-256 digest of owner, bytecode, maximum local slots,
string-constant admission policy, and every constant-pool entry in index order.
Entries have explicit type tags and lengths. Float/double constants preserve
their exact bits, including signed zero and NaN payloads. Old names miss rather
than loading an object with incomplete input identity; existing files are not
deleted. Both cache lookup paths use the same key function.

## Validation

Candidate SHA-256:
`ED932C965687A278AB60082B078A72AB4EF42BA03401CD424CE29AA82B4BDE7E`.
The release build succeeds and all 29 `aot_clif` unit tests pass, including three
new key tests for pool changes, float bits, local-slot counts and policy changes.

The executable probe passes version order 1, 2, 2, 1 with expected results
100000000, 200000000, 200000000, 100000000. AOT execution is confirmed in stderr.
After both versions populate the cache, repeated versions preserve its file set,
sizes and write timestamps. Evidence, including javap output proving identical
instruction bytes, Java references, executable hash and environments:
`%TEMP%/rava-aot-pool-cache-e54a3c91403f49bd9894926effd229dd`.

This is a cache correctness prerequisite, not an offline-build implementation
or a startup speedup. Default execution does not enable this AOT cache. The
legacy build namespace still uses executable modification time; replacing that
with a stronger compiler/target compatibility identity remains necessary before
claiming a durable, portable artifact contract. Runtime linking, class-init/GC
guards and current scheduling restrictions also remain.
