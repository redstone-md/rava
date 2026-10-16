# Contributing to rava

Bug reports, fixes, and small programs that behave differently under rava than under `java`
are all welcome.

## Reporting a problem

[docs/troubleshooting.md](docs/troubleshooting.md#reporting) lists what a report needs: the rava
version, the system, how the program was started, the full output, and a thread dump for a
hang. If you can, also run the program with `java` (HotSpot) of the same Java version.

## Building and testing

You need stable Rust and JDK 21. The tests compare against its `java`, and a development build
borrows its class library.

```sh
cargo build --release          # target/release/rava
tests/check.sh                 # semantic probes, the JNI test and the patches test
tests/check.sh all             # also with every method in the optimizing tier
```

CI runs `tests/check.sh` on Linux, macOS and Windows. The script compiles every probe in
[tests/semantics/](tests/semantics/), runs it with `java` and with rava, and expects the same
output from all but four probes. Those four (`ExcSem`, `JitNpeSiteSem`, `JitThrowNullSem`,
`NpeSem`) check HotSpot's helpful `NullPointerException` messages, which rava does not
implement yet. `SEM_DIFF=1` prints how a failing probe differs.

On Windows, run the same commands from Git Bash. The JNI test needs MinGW's `gcc` on `PATH`
(`CC=gcc`). You can also cross-compile a Windows build on Linux
(`cargo build --release --target x86_64-pc-windows-gnu`) and run it under Wine with
`JAVA_HOME` pointing at a Windows JDK.

## Adding a semantic probe

A probe is one Java file in `tests/semantics/` whose `main` prints what it observes. Its output
must be the same on every system and every run, so it prints no addresses, hash codes, timings
or thread interleavings. The first line is a comment saying what it checks. A bug fix should
come with the probe that would have caught it.

## Changes to the VM

The rules in the [README](README.md#rules) are not negotiable:

- No workload-specific tuning. Nothing is keyed to one program's classes, methods, names or
  outputs. Generic adaptivity (counters, tiers, profiles, inline caches, speculation with
  deoptimization) is fine, and so are intrinsics for `java.*` methods.
- No snapshots. Every start is a cold start from class files.
- Drop-in `java`. The `java` command line works unchanged, and nothing may require an extra
  flag.

Changes to the compilers or the collector need more than the probes. They also need a Paper
server start with `bench/paper_ab.sh HEAD java` (the script's header describes the one-time
setup), which measures the effect on start-up time as well. Report numbers from alternating A/B
rounds on the same host, not from single runs.

## Code

- Match the surrounding code: its naming, comment density and idiom. Comments and doc
  comments explain why, in English.
- Keep files focused and under about 500 lines. When one grows, split it by responsibility.
- Platform-specific code goes behind the platform layer (`crates/rava/src/os/`) or in
  per-system modules (`natives/windows/`, `natives/sock/unix.rs`). Do not scatter `cfg`s.
- Every `unsafe` block carries a `// SAFETY:` comment explaining why it is sound.

## Commits and pull requests

- One logical change per commit. The subject follows
  [Conventional Commits](https://www.conventionalcommits.org) and is in English
  (`fix(natives): ...`, `feat(jit): ...`, `docs: ...`). Add a body that explains why when it is
  not obvious.
- Pull requests target `main`, and CI must pass on all three systems.
- Commits and pull requests carry no attribution lines for tools.

By contributing you agree that your work is licensed under the [Apache License 2.0](LICENSE)
and that you are credited as one of the rava contributors ([NOTICE](NOTICE)).
