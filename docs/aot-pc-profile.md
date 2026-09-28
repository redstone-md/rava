# Prepared-code PC attribution

The existing PC sampler records JIT method address ranges at publication.
The prepared AOT reader did not register its executable ranges, so samples in
those methods could not be attributed to their guest identity.

Expose the linked function's exact byte length and register its range once,
when a successful AOT cache entry is published. Guard identity allocation and
registration with the existing diagnostic flag. Do not change emitted machine
code or method admission. Profile a build with symbols and linker ICF disabled;
use a separate uninstrumented release for performance verdicts.

Next validation: check compilation and AOT semantics, then collect a complete
prepared/no-JIT boot profile and distinguish guest machine-code execution from
interpreter, callback and host dispatch costs. This diagnostic is not evidence
that the five-second target has been reached.

All 49 AOT tests passed. A diagnostic release built with wall-profile and
`-C link-arg=/OPT:NOICF` ran a separately prepared integer-loop probe with JIT
disabled. Its result matched Java (-719357952), and 28 PC samples were attributed
to AotProfileProbe.work. Evidence:
`%TEMP%/rava-aot-pc-probe-16e06fd1b75b4045b7d0eeaf7ce3de08`.
Full-workload profiling remains pending; probe attribution is not a timing gain.

## Full diagnostic run

Saved diagnostic release SHA256:
`5F75A5DF58A69AB9080C8D9DDB44297AA0D5F010D94B52B54EF8E5E0CE76CF83`.
Matching prepared objects and analysis scripts:
`%TEMP%/rava-aot-pc-server-c2c94116b613414f918a90bb1a8bee23`.
Boot evidence: `%TEMP%/rava-product-boot-a80505deb5e74d9dabfdaa28b73fa540`.
RAVA_JIT=0, PC and wall profiling enabled, fresh world, no snapshot.
Done was reached in 193.548 seconds, CPU 191.5625 seconds. This is an
instrumented diagnostic, not a release performance verdict.

Filtering PC timestamps through Done gives 158296 samples: 2186 in registered
prepared code (1.38%), 104442 in rava_interp symbols, 51668 in host or unresolved
symbols. The legacy `<jit>` trace label includes prepared code here; it does not
mean JIT was enabled. These are samples of the selected guest thread, not a
whole-process CPU decomposition. Its waiting tail is visible in the results.

Leading self samples: NtCreateFile 12.89%, ZwWaitForAlertByThreadId 10.45%,
jit_call_body 5.36%, invoke_framed 4.97%, invokevirtual 3.06%, jit_direct_call
2.62%. Symbol percentages do not price entire call subtrees. Nevertheless,
optimizing the prepared method instruction stream alone addresses little of
this observed execution. The next implementation should target the call
boundary: a statically resolved prepared callee accepting rooted raw cells,
without the synthetic interpreter call frame and Value-to-cell round trip.
It must preserve class initialization, reference forwarding, pending exceptions,
boxed result lifetime and native-backed representation guards. Method names
from the diagnostic must not become admission rules.
