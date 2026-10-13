# Contributing to rava

Thanks for helping. Bug reports, small programs that behave differently under rava than under
`java`, and fixes are all welcome.

## Reporting a problem

See [docs/troubleshooting.md](docs/troubleshooting.md#reporting): the rava version, the system,
how the program was started, the full output, and for a hang a thread dump. If you can, check
the same program with `java` (HotSpot) of the same Java version.

## Building and testing

You need Rust stable and a JDK 21 (its `java` is the reference the tests compare against, and a
development build borrows its class library).

```sh
cargo build --release          # target/release/rava
tests/check.sh                 # semantic probes, the JNI test and the patches test
tests/check.sh all             # also with every method in the optimizing tier
```

`tests/check.sh` is what CI runs on Linux, macOS and Windows. It compiles every probe in
[tests/semantics/](tests/semantics/), runs it with `java` and with rava, and expects the same
output from all but four probes (`ExcSem`, `JitNpeSiteSem`, `JitThrowNullSem`, `NpeSem`, which
check HotSpot's helpful `NullPointerException` messages, not implemented yet). `SEM_DIFF=1`
prints how a failing probe differs.

On Windows, run the same commands from Git Bash with MinGW's `gcc` on `PATH` (`CC=gcc`) for the
JNI test. A Windows build can also be cross-compiled from Linux
(`cargo build --release --target x86_64-pc-windows-gnu`) and run under Wine with
`JAVA_HOME` pointing at a Windows JDK.

## Adding a semantic probe

A probe is one Java file in `tests/semantics/` whose `main` prints what it observes. It should
print the same on every system and on every run (no addresses, hash codes, timings or thread
interleavings). The first line is a comment saying what it checks. A bug fix should come with
the probe that would have caught it.

## Changes to the VM

The rules in the [README](README.md#rules) are not negotiable:

- **No workload-specific tuning.** Nothing keyed to one program's classes, methods, names or
  outputs. Generic adaptivity (counters, tiers, profiles, inline caches, speculation with
  deoptimization) is fine, as are intrinsics for `java.*` methods.
- **No snapshots.** Every start is a cold start from class files.
- **Drop-in `java`.** The `java` command line works unchanged; nothing may require an extra
  flag.

Changes to the compilers or the collector are checked beyond the probes: a Paper server start
with `bench/paper_ab.sh HEAD java` (see the script's header for the one-time setup), which also
measures the effect on start-up time. Report numbers as alternating A/B rounds on the same
host, not single runs.

## Code

- Match the surrounding code: its naming, comment density and idiom. Comments and doc
  comments say why, in English.
- Keep files focused and under about 500 lines; split by responsibility when one grows.
- Platform-specific code goes behind the platform layer (`crates/rava/src/os/`) or in
  per-system modules (`natives/windows/`, `natives/sock/unix.rs`), never as scattered `cfg`s.
- `unsafe` blocks carry a `// SAFETY:` comment saying why they are sound.

## Commits and pull requests

- One logical change per commit, with a [Conventional Commits](https://www.conventionalcommits.org)
  subject in English (`fix(natives): ...`, `feat(jit): ...`, `docs: ...`) and a body that says
  why when it is not obvious.
- Pull requests target `main`; CI must pass on all three systems.
- No generated attribution lines in commits or pull requests.

By contributing you agree that your work is licensed under the [MIT License](LICENSE).
