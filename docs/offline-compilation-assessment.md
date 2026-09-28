# Offline compilation: current implementation boundary

Source inspection on 2026-09-05; this is an assessment, not an implemented
replacement for JVM startup. The user proposed JVM bytecode to Rust translation.
Compiling that generated Rust ahead of execution would be an AOT design.

## Existing reusable parts

- `aot_clif` produces relocatable code and resolves runtime callback symbols.
- `cnp::CompiledObj` serializes code with relocations and constant descriptors.
- `vm/aot_link.rs` resolves per-process constants rather than persisting their
  addresses, then links the object.
- `llvm_aot` emits LLVM IR through clang for a numeric subset. Its current
  object extraction refuses text relocations; it is not a full application
  compiler.

## Why enabling current AOT is insufficient

`Vm::aot_run` still increments `aot_hot`. Ordinary admission waits for
`jit_threshold`; `RAVA_AOT_FROM_CALL1` additionally admits a first call when an
object already exists on disk. That selects execution using previous workload
history and is not the required no-adapt offline build contract.

`aot_compile_link` refuses methods with exception tables. String/class constants
also have restrictions. Execution guards protect unfinished class initialization,
GIL-free workers, and native-backed object representations. Removing those
guards does not implement the missing semantics.

Generating Rust that calls the same general runtime helpers would retain their
dispatch, representation and allocation costs. Generating typed direct operations
requires compiler proofs and a compatible runtime, regardless of output language.

## A valid next experiment

If pursuing offline compilation, build objects from an explicit input classpath
without running the guest or using execution profiles. Resolve runtime addresses
in a fresh process, preserve class initialization order, and keep unsupported
methods explicit. Artifact identity checks must not decide which guest methods
deserve optimization based on previously observed execution.

Use the existing relocatable backend for a bounded end-to-end probe before
creating another emitter. Compare the same code's semantics and full fresh
startup. Record offline build time separately. Persisting initialized application
objects, registries, or completed reload results is outside the no-snapshot goal.
Neither a Rust emitter nor the existing AOT modules currently prove startup below
five seconds.

## Class-file compiler boundary

`aot_clif::ClassCompiler` now accepts an immutable parsed `ClassFile` and a method
index and returns a relocatable `CompiledObj`. It does not create a VM, execute
class initialization, link runtime addresses, or read/write the object cache.
The existing VM AOT path delegates to this compiler, preserving its runtime
guards, cache identity, and per-process linking. The public compiler refuses
string/class literals and exception handlers; the existing unsafe diagnostic
override remains crate-private.

The isolated test fixture has an infinite-loop `<clinit>`. Its numeric method
is compiled, serialized, deserialized, linked, and executed without creating a
VM. Additional tests cover missing methods/code, class literals, and exception
handler refusal. This verifies code preparation, not Java initialization in a
complete runner. The subsequent explicit classpath build and read-only object
loading experiment is documented in `prepared-aot.md`; full application
semantics, complete no-adapt execution, and the wall target remain outstanding.

Validation on 2026-09-05: all 32 `aot_clif` tests pass, release builds, and
`bench/aot_pool_cache_sem.ps1` passes versions 1/2/2/1 with outputs
100000000/200000000/200000000/100000000 and object counts 1/2/2/2. Evidence is
`%TEMP%/rava-aot-pool-cache-5d3c65b8e96344f4b4a1a24c49050644`; release SHA-256 is
`3C756722C53D8922C8EB6BAF458971AA3DA6E3E92606BA29A93CFEFEA5A6A13A`.
This extraction has no full-boot performance measurement or speed claim.

## Verified prerequisite correction (2026-09-05)

Full-AOT cache identity now includes the constant pool and translator input
layout/policy. The old key reproduced a silent wrong result after a same-class
literal change with unchanged bytecode; the corrected shared-cache round trip
passes. See `aot-pool-cache-identity.md`. This does not remove the scheduling,
runtime-guard, or complete artifact-compatibility limitations above.
