# Prepared execution without JIT

The full prepared lane previously required Vm.jit_enabled. Consequently,
RAVA_JIT=0 disabled prepared execution as well, even though those methods had
already been compiled offline. A differential CfgStackSem run matched Java
but failed its prepared-entry assertion, confirming interpreter fallback.

Allow an explicitly supplied prepared directory to enter the AOT lane with
JIT disabled. Preserve all representation, initialization, exception and worker
guards in aot_run. Keep the separate jit_run gate unchanged. force_compile
already refuses when Vm.jit_enabled is false.

Validate matching Java output and actual prepared entries with RAVA_JIT=0.
Then validate the full prepared workload and measure it separately from the
high-threshold diagnostic. Disabling JIT does not by itself certify that every
other runtime shortcut is independent of execution history, nor prove wall <5s.

The release probe passed with RAVA_JIT=0 and no threshold override: boolean,
reference, wide, switch and null results matched java -Xint, and all four checked
helper methods entered prepared execution. Run it with
`bench/prepared_without_jit_sem.ps1 -Exe <release-executable>`.
Evidence: `%TEMP%/rava-prepared-nojit-beaf95c1cf634575a61e19f564a86bdf`.

Full prepared validation used release SHA256
`7D49230D4096BCB70533D98B11F568AD66144481983DE0C0555AA7E4C56A2D31`
and fresh objects under
`%TEMP%/rava-prepared-nojit-server-dfdd0b30ccee4a7fbb16f62e44e29c53`.
33 of 34 probes matched. CompSem exceeded the 120-second RAVA process timeout;
Java completed normally. Its outputs are preserved in `comp-timeout` under that
directory. The validator stopped before product gates or boot measurement.
Diagnostic compilation overlapped part of validation, so this timeout alone is
not an isolated performance estimate or proof of semantic divergence.

Next evidence is a bounded diagnostic no-JIT boot with prepared-code PC
attribution, under `%TEMP%/rava-aot-pc-server-c2c94116b613414f918a90bb1a8bee23`.
It is diagnostic only and cannot replace uninstrumented correctness and timing.
