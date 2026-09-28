# Arena String semantics at native boundaries

Expanded prepared coverage exposed three places that treated only Value::Str
as a Java String. Collection element hashing fell back to object identity for
arena strings, the case-insensitive comparator fell back to natural order,
and cast messages exposed the representation's `[arena-string]` class name.

Collection hashing now uses the existing cached arena string hash. Comparison
uses the existing str_bytes view for both representations before applying the
existing case-folding operation. Cast diagnostics classify arena strings as
java/lang/String before forming the Java class/module message. This preserves
the representation rather than converting all prepared strings on every call.

All three regression tests failed before the changes and pass afterward:
element/list hashes, mixed-representation comparisons, and the complete cast
message. Logs: `%TEMP%/rava-arena-string-contract-before.log` and
`%TEMP%/rava-arena-string-contract-after.log`. The tests cover ASCII and BMP
hash examples; they do not claim to repair all preexisting Unicode semantics.
The release passes ArraysSem, EqualsSem, ExcSem, GuestListSem and IterSem with
expanded prepared constants enabled, together with the iterator witness fix.
Evidence: `%TEMP%/rava-string-boundary-probes-252923412da1484fbbe360c2a14f1e69`.
All 71 AOT-selected library tests pass (one ignored), and the release literal
identity/initialization/GC/configuration probe passes at
`%TEMP%/rava-aot-literals-8cce66caa77441498b04552c27de8f30`.
Frozen executable SHA256:
`C682F5C30A0E601CE86BAF04333AA9A54C176E422D0490884ED8D81AEE4B41A7`.
The full 39-probe differential suite now matches Java -Xint. Sequential fresh
world boots on this executable measured prepared AOT at 140.774 s wall,
138.46875 s CPU, and ordinary JIT at 24.429 s wall, 26.53125 s CPU.
Evidence directories under `%TEMP%`:
`rava-product-boot-6cd1c59846564b0d837d095259de9755` (prepared) and
`rava-product-boot-fd1ef9bc693f49b4b72334c5615e2041` (ordinary JIT).
These are single observations; quiet performance counters were unavailable.
The original 27-30 s baseline remains the relevant comparison. Prepared AOT
is a severe regression, not a recovery relative to the original baseline.
Ordinary JIT has not been certified no-adapt, and the sub-5 s goal remains
unmet. Full product workload validation, including ReloadBench, is incomplete.
