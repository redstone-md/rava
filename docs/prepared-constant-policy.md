# Prepared constant-policy agreement

The offline builder always constructed the default ClassCompiler, ignoring
RAVA_AOT_UNSAFE. Runtime lookup did include that flag in each object key. Enabling
the flag therefore produced misses even for otherwise covered methods, while
literal-bearing methods were never compiled. A Java-equivalent result alone
hid this interpreter fallback; requiring AOT entry traces exposed it.

The builder now applies the explicitly selected policy and records it in a
configuration file before publishing the completion marker. Runtime validates
that file against its own policy. Legacy artifacts without a configuration file
remain compatible with the default policy only. Different policies fail
explicitly instead of silently selecting the interpreter for all methods.
Executable identity and method-key validation remain unchanged.

The flag stays off by default. The literal probe requires actual prepared
execution, string/class identity, class-literal initialization ordering, live
GC with retained references, and explicit rejection of a mismatched policy.
All 71 AOT-selected library tests pass (one ignored). The rebuilt release passes
the literal probe, actual AOT entry checks and GC, and the default-mode
directory/JAR preparation and direct dispatch probes. Literal evidence:
`%TEMP%/rava-aot-literals-f588585bc8f44cc19329b83e2c459171`.
Default-mode evidence: `%TEMP%/rava-prepared-a460259b4042489a85dd0d5a28b2420a`.
Dispatch/GC evidence: `%TEMP%/rava-aot-dispatch-b59a47c99f614a9188b6bc7c723c9701`.
Logs: `%TEMP%/rava-constant-policy-tests.log`, `rava-constant-policy-build.log`,
`rava-constant-policy-literals.log`, `rava-constant-policy-default.log`,
`rava-constant-policy-dispatch.log`. Expanded application validation and timing
remain outstanding; the flag is still off by default.

Full expanded preparation completed with 30,464 classes, 311,889 compiled
methods and 37,186 refusals in
`%TEMP%/rava-constant-policy-server-8ddd1427f2b044d1ae66601381a9b841`.
The `%TEMP%/rava-constant-policy-server-current.txt` pointer identifies it.
The previous default-policy set had 30,462
classes, 292,541 compiled methods and 56,526 refusals; the new classpath also
includes the two literal-probe classes. The full semantic run is terminal:
34 of 39 probes match and five diverge. The pipeline stopped before boot;
there is no expanded-mode wall result.

| Probe | Newly exposed mismatch |
| --- | --- |
| ArraysSem | case-insensitive sort becomes natural-order sort |
| EqualsSem | deep array hash and Objects.hash differ |
| ExcSem | class-cast message exposes `[arena-string]` instead of Java String |
| GuestListSem | missing concurrent-modification exception and unequal hash |
| IterSem | next after add/remove fails to throw concurrent-modification exception |

The unchanged executable, objects and environment with only
`RAVA_NO_JIT_ITER_SHORTCUT=1` makes IterSem pass and removes GuestListSem's CME
mismatch; GuestListSem's hash mismatch remains. These controls are saved as
`IterSem-no-shortcut.log` and `GuestListSem-no-shortcut.log`; original outputs
are preserved in `before-iterator-control`. No source fix for these failures
has been applied yet.

The shortcut in vm/jit_call.rs advances `$pos` without the ExpSize/Source check
in stream/collections/mod.rs's normal iterator path. Reuse that semantic check
before advancing. Separately, vm/iterate.rs's ciorder arm accepts Value::Str
only, although its existing str_bytes helper also recognizes arena strings.
These are concrete investigation points, not proof that all five failures
share one cause. Keep expanded constants off by default until corrected and
validated; passing the small literal probe was insufficient.
