# First-entry PC profile after queue integration

Source: d2a176db, explicit first-entry JIT and cap 16384, default early queue
reservation and standard register allocation. The goal remains full startup
wall below five seconds, no adaptive eligibility and no snapshots.

Built successfully with release-diag, next-gen-gc, no wall-profile feature,
and `/OPT:NOICF`. Executable and PDB were copied together into the isolated
fresh-fixture boot directory:
`%TEMP%/rava-product-boot-3dedcf3ad87b49caa4a55307a7439dff`.

- Executable SHA256: 3E7A0C11181F3FE8AFC4F424F99772DF4955FF932B760424AF844C52E526E619.
- PDB SHA256: FA648BB0150BD4B42FCA6927AE1D57004CA2052EF88A5A0F8AC7F5B06F357FB1.
- Full Done reached; instrumented wall 26.667 s, CPU 47.109375 s.
- 19551 primary-thread samples inside the recorded launch-to-Done window.
- First sample 565 ms after launch; post-Done samples excluded.
- 51 unresolved `<host ...>` samples (about 0.26%).
- 2655 samples in registered guest JIT ranges (about 13.58%).

Neither instrumented wall nor these primary-thread counts are all-thread CPU
attribution. PC profiling still changes execution through its instrumentation
and region guards. Quiet counters were unavailable. Unregistered guest code,
compiler helpers, allocation, I/O and runtime work cannot all be treated as
removable interpreter overhead.

| Grouped leaf | Samples |
| --- | ---: |
| LocalKey::with (all instantiations) | 620 |
| mi_malloc_aligned | 559 |
| Vm::jit_call_body | 500 |
| memchr | 486 |
| Vm::jit_direct_probe | 445 |
| NtCreateFile | 413 |
| Vm::jit_run | 366 |
| Heap::gc_with_object_slot_policy | 362 |
| dispatch_owner_cascade | 339 |
| Vm::fu_fastpath | 334 |
| Vm::jit_direct_call | 314 |

The generic-stripping report combines LocalKey instantiations. Inspecting raw
symbols attributes 199 samples to the valkey fold closure, 85 to hr_synth plan
lookup, and 49 to argument-layout lookup. These include inlined closure work;
they do not price the TLS access instruction itself. The valkey implementation
already uses a const TLS initializer, UnsafeCell, compact spans and a contiguous
byte buffer. Do not reimplement those optimizations or interpret the combined
620 samples as one cache to remove.

The call boundary remains a concrete investigation target after the first-entry
cap correction. Existing native argument pools, superclass plans and late-new
slots have already been audited. A further change must remove repeated work at
the compiled/native boundary while preserving root and exception contracts;
a new table alone is not evidence of progress toward the five-second goal.

Raw analysis scripts, samples and grouped JSON are in the directory referenced
by `%TEMP%/rava-first-entry-pc-current.txt`. The analyzer reuses the repository's
symbol demangler and filters against result.json timestamps. Its initial ad-hoc
unknown-symbol check missed the `<host ...>` spelling; the corrected count is
51 above, not zero. The normal release executable remains 858AEA5B; diagnostic
builds did not replace it.
