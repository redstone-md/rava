# PGO build for RAVA

Profile-guided optimization of the RAVA binary itself (the Rust engine — not the
guest JIT). Semantics are untouched: same source, same features, same
`target-cpu=native`, only LLVM's block layout / inlining / branch weights change.
`ChunkFill_HASH=8790f04ace726ecf` holds on the training runs and on the product.

## Why it is not trained on `net.minecraft.server.Main`

Two hard blockers, both measured rather than assumed:

1. **LLVM writes the profile from an at-exit handler**, and RAVA's server boot has
   no reachable clean exit. `bench/stdin_stop_test.ps1` pumps stdout/stderr
   asynchronously and writes `stop` to the real stdin pipe after `Done (`: the
   server reaches Done at 140 s and is still alive 60 s later. Nothing consumes
   the console command.
2. **LLVM continuous mode (`%c`) does not survive here.** The counters do land in
   the file (5.2 MB after a kill, `bench/pgo_contmode_test.ps1`), but
   `llvm-profdata merge` rejects it: `truncated profile data / no profile can be
   merged`.

So training uses the two canonical drivers over the SAME `server.jar`, both of
which return from `main` and exit:

| driver | covers |
|---|---|
| `BootPhaseC` | `Bootstrap.bootStrap` → datapack load = blocks A+B+C, the serial prefix (~2/3 of boot wall) |
| `ChunkFill 1 64` | the worldgen ring = the Done window |

What is NOT covered is only `Main`'s own port bind and level-prep glue.

## Training on the REAL boot (2026-08-07)

Both blockers above are about the GUEST not cooperating — no clean exit, no way to
ask it to stop. Neither is about the profile runtime, which will write its counters
whenever it is asked. `RAVA_PGO_DUMP_AFTER_MS` (cargo feature `pgo-dump`, rava-cli)
arms a timer that calls `__llvm_profile_write_file` and then `exit`s, so the boot can
be the training workload directly and the drivers stop being a proxy for it.

```powershell
$env:RUSTFLAGS = "-C target-cpu=native -C profile-generate=D:\code\rava\target\pgo-boot-data"
cargo build --profile release-fat -p rava-cli --features next-gen-gc,pgo-dump `
      --target x86_64-pc-windows-msvc
Remove-Item Env:RUSTFLAGS
.\bench\pgo_train_boot.ps1 -DumpAfterMs 210000 -TimeoutSec 300
```

For a fresh production boot profile, pass an isolated run root to the orchestrator:

```powershell
$run = 'C:\path\to\isolated-rava-pgo-run'
.\bench\build_prod.ps1 -BootTrain -RunRoot $run
```

The run root must contain `out`, `world_pristine`, `server.properties` with port
25599, `eula.txt`, and `rava.toml`. The scripts refuse a fresh boot-training run
without an explicit root, reject the interactive port 25565, and stop if the
benchmark port is already occupied. Profile data stays under this checkout's
`target` directory, so parallel checkouts cannot silently reuse each other's data.

The feature is training-only: `__llvm_profile_write_file` exists only in a
`-C profile-generate` build, so a shipped build must NOT carry it.

⚠ **Size the window against the INSTRUMENTED binary, not the product.** It runs roughly
2× slower, and a dump that lands before `Done (` trains on the serial prefix alone —
a profile that looks fine and silently over-weights class loading. `pgo_train_boot.ps1`
refuses that case rather than merging it; the first attempt at 110 s hit exactly it,
and 210 s reached Done with a 5.38 MB `.profraw`.

## The trap that costs a whole training cycle

`rava-cli`'s fast exit (`main.rs`, `std::process::exit` skipping `Drop`, opt-out
`RAVA_NO_FAST_EXIT`) leaves a **0-byte `.profraw`** — the at-exit writer never
completes. Same guest program, measured back to back
(`bench/pgo_write_probe2.ps1`):

```
hello         (fast exit)          0 B
hello-nofast  (RAVA_NO_FAST_EXIT)  5,243,832 B
```

`bench/pgo_train.ps1` therefore sets `RAVA_NO_FAST_EXIT=1`. It changes teardown
only, so the counters describe the same run.

## Procedure

One command: `.\bench\build_prod.ps1` (≈13 min), then gate it with
`.\bench\pgo_gates.ps1 -Exe target\pgo-fat\rava.exe`. The steps it runs:

```powershell
# 1. instrumented build (~4.5 min)
$env:RUSTFLAGS = '-C target-cpu=native -C profile-generate=D:\code\rava\target\pgo-data'
cargo build --profile release-fat -p rava-cli --features next-gen-gc --target x86_64-pc-windows-msvc
Remove-Item Env:RUSTFLAGS

# 2. training runs (~2.5 min) — compiles the drivers into out/ on first use
.\bench\pgo_train.ps1

# 3. merge
$pd = "$env:USERPROFILE\.rustup\toolchains\stable-x86_64-pc-windows-msvc\lib\rustlib\x86_64-pc-windows-msvc\bin\llvm-profdata.exe"
& $pd merge -o target\pgo.profdata (Get-ChildItem target\pgo-data\*.profraw).FullName

# 4. optimized build (~6 min)
$env:RUSTFLAGS = '-C target-cpu=native -C profile-use=D:\code\rava\target\pgo.profdata'
cargo build --profile release-fat -p rava-cli --features next-gen-gc --target x86_64-pc-windows-msvc
Remove-Item Env:RUSTFLAGS
Copy-Item target\x86_64-pc-windows-msvc\release-fat\rava.exe target\pgo-fat\rava.exe -Force
```

The explicit `--target` keeps `RUSTFLAGS` off build scripts; because it also
disables `.cargo/config.toml`'s `[build] rustflags`, `target-cpu=native` has to be
repeated in `RUSTFLAGS` — and is, in both builds.

Merged profile: 15,800 functions, 250,530 blocks, 64.0 G total counts.
Binary size 13,049,344 B (`release-fat`) → 12,459,008 B (PGO).

## Measured effect

See the boot-speed ledger. Read the contamination note there before quoting any
absolute wall number from 2026-08-04.
