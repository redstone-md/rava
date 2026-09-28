# Current JIT caller attribution

2026-09-06, source 1dca4a33 including the ThreadLocal storage fix.
The preceding handler-refusal experiment was rejected, so another refusal-cache
change was not attempted. Handler integration audit confirmed that the generic
rava_deopt adapter cannot be reused unchanged: it maps nonterminal RunExit and
non-Throw errors to zero. ExceptionSite::resume_pending deliberately preserves
those outcomes. Admission remains closed; no dormant adapter was enabled.

WPR CPU capture failed before recording with 0xc5585011 (system performance
profiling policy could not be enabled). WPR status subsequently confirmed no
recording. The existing RAVA stack-copy profiler was used without source changes.

Evidence: `%TEMP%/rava-etw-stacks-64f76ddd7e4b45e3b1f3c14bae5d7117`.
Build: release-diag, next-gen-gc, /OPT:NOICF, without wall-profile.
Executable SHA256: `38E580DFE5473C61ECDDF3BF157FCD6E6D702EE3CE2995199D46425AE9404706`.
PDB SHA256: `F36CB3FCC88DBD50298AD27D661737064E72D52C830165FF0CA2114537FDD838`.
Actual run executable hash matched. Fresh-world run:
`%TEMP%/rava-product-boot-b35bc11cd7964f0792b9b6ccee5e94e8`, PID18412.
Instrumented Done: 30.121 seconds, CPU66.78125 seconds. These are profiling costs,
not a speed comparison. Ordinary release remains 05ADCBE8... unchanged.

The trace was filtered to launch/Done epochs before analysis. It contains 12227
primary-thread samples, first at launch+371ms; 1789 samples name guest JIT code.
9079 samples have at least one candidate caller whose direct-call target matches
the sampled function. This is conservative stack scanning, not a true unwind:
matching stale addresses can survive, indirect calls truncate chains, and sample
counts cannot be summed into removable wall cost. Compiler threads are not
represented by these primary-thread rows; process CPU includes them and sampling.

| Sampled leaf | Candidate immediate caller | Samples |
| --- | --- | ---: |
| Heap::gc_with_object_slot_policy | Vm::gc | 304 |
| Vm::jit_call_body | Vm::jit_call | 297 |
| Vm::fu_fastpath | Vm::jit_fu_invoke | 289 |
| Vm::jit_direct_probe | Vm::jit_direct_call | 264 |
| LocalKey::with | keys::combo_entry_bytes | 201 |
| LocalKey::with | casts::checkcast_reject | 108 |

Source audit confirms the key chain:
combo_hash_arena_array_map -> combo_entry_bytes -> valkey::fold. A memo hit still
walks all stored key bytes through combo_fnv; the caller initializes the key seed
to the FNV offset basis, while the value seed depends on preceding key bytes.
This identifies byte folding inside a TLS closure, rather than proving TLS lookup
itself costs the entire row. A content-derived cached initial-seed fold is a
candidate to price; arbitrary-seed value folding must retain its exact behavior.
No optimization or speedup is claimed by this profile.

checkcast_reject's matching chain is rava_checkcast -> checkcast_reject -> the
CCAST_MEMO lookup. Its result already caches the class/site verdict. This is not
evidence that repeated full hierarchy traversal accounts for those samples.

Raw trace, frozen binary/PDB, hash manifest and temporary callers.py/callers.json
are retained in the evidence directory. No tests were added or run in this pass.
The full wall<5s/no-adapt/no-snapshot objective remains unmet.
