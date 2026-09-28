# HANDOFF — read this before touching anything

Written 2026-09-04 at the end of a long session. It exists so the next agent does not re-learn what
this one paid for. `docs/the-boot-price-map.md` is the full ledger; this is the part you need first.

---

## 1. HARD RULES — violating any of these wastes the session

**⚠️ NEVER kill `java` broadly.** The user runs their own Minecraft on this machine. Kill only a
stray `rava.exe` you started, by PID. Port **25565 is the user's server**; benchmarks use 25599.

**⛔ SNAPSHOTS ARE FORBIDDEN as a speed path.** Checkpoint/restore does not count as booting fast.
`RAVA_RESTORE`, `RAVA_CHECKPOINT_*` are unset by every harness on purpose.

**⛔ NO-ADAPT.** Production eligibility must never be routed by: class/method/package **names**,
workload names, body hashes, inputs, outputs, expected hashes, **timings**, or prior aborts.
Permitted evidence: **bytecode, CFG, descriptors, resolved dispatch, effect summaries, structural
facts established at class load.** A census keyed by name is fine — a *decision* keyed by name is not.

**Talk to the user in Russian.** Commit messages and code comments in English.

**Builds:**
```
cargo build --release -p rava-cli --features next-gen-gc              # the product binary
cargo build --profile release-diag -p rava-cli --features next-gen-gc,wall-profile   # symbols for profiling
```

**Base environment** for every product run:
```
RAVA_JIT=1 RAVA_JIT_TIER0=1 RAVA_PROC_CAP_MB=4096 RAVA_HARD_MB=3584
```

`RAVA_GSON_NATIVE=1` is a name-specific route and must remain unset for strict
no-adapt runs. The former `RAVA_M3_BITSTORAGE` shortcut has been removed from
runtime code; historical benchmark scripts may still pass its now-ignored value.

---

## 2. HOW TO MEASURE — the most expensive lesson of the session

### 2.1 The boot harness cannot resolve anything smaller than ~17%

Null test on an **idle** box (CPU 4-11% busy, 7.5 GB free, nothing running): eight boots of the same
binary with the same configuration, the "flag" a name no code reads —

```
wall  ON  29.4  26.8  28.8  30.7      OFF  29.1  31.5  27.0  28.3
```

**27.0 to 31.5 within one arm = 16.7%.** The noise is not contention, it is **this engine's own
non-determinism**: the background compiler gets through a different set of methods every run, so
every boot does physically different work.

⇒ **Every "neutral" verdict in this tree that was measured by a boot A/B is INVALID and must be
re-priced.** Two were re-priced this session and both changed: `RAVA_BG_JIT_THREADS=4` went from
"neutral" to **DISJOINT −2.9%** (shipped), `RAVA_ARENA_NATIVE_NEW` went from "nothing" to a
**DISJOINT loss**. There are more waiting.

### 2.2 Use the warm reload — A-arm spread 0.9-1.8%

```
bench\reload_ab.ps1 -Flag NAME [-FlagValue V] -Pairs 4 -Exe target\release\rava.exe
```
Metrics: `RELOAD_MS` (cold — carries compilation), `RELOAD2_MS` (warm — the steady state),
`WINDOW_MS`, `cpu_ms`, `cycles`. Ten times better resolution than the boot on the same box.

`bench\boot_ab_flags.ps1 -Flags A,B -Rounds N` still exists; use it only for effects above ~20%.
Linux ports: `bench/reload_ab.sh`, `bench/boot_ab_flags.sh`.

### 2.3 Rules that cost retractions to learn

1. **Interleave, or measure nothing.** An A/B whose arms run in sequence measures WHEN they ran. Two
   false findings in one hour came from sweeps where the interesting arm ran last.
2. **A share in a profile is NOT a price until removing the work moves the clock.** Six retractions
   this session. Convert to a share of the whole workload *before* writing code.
3. **DISJOINT is the only verdict noise cannot manufacture** — every B below every A. Quote signs,
   not magnitudes, when arms overlap.
4. **Null test first, on every fresh session**, against a flag no code reads. It tells you what the
   box can resolve today.
5. **A boot with no `Done` line is not a data point.** The harness drops such rounds; do not average
   them back in.
6. **Verify the UNSET path after flipping a default.** One flip crashed 5/5 because two derivations
   of one fact disagreed exactly when the variable was unset.

### 2.4 Verification bar — run all of it before shipping

```
bench\pgo_gates.ps1 -Exe target\release\rava.exe     # 4 bit-exact hashes, MUST pass the -Exe arg
bench\sem_all.ps1  -Exe target\release\rava.exe      # 23 differential probes vs java -Xint
```
Expected: `chunkfill 8790f04ace726ecf`, `ringscan/ringlane 37539c6c375d0c3`,
`nbtsave ae44a34b42766bd4`, `ReloadBench HASH=c956377e19071d7a`,
`BootstrapBench HASH=b9e7debe631b42b6`.

⚠️ `pgo_gates.ps1` defaults to a stale PGO binary. **Always pass `-Exe`.**

---

## 3. WHERE THE ENGINE ACTUALLY STANDS

Quiet box, three interleaved pairs:

| | rava | java | ratio |
|---|---|---|---|
| boot wall | 25.2 s | 6.3 s | **4.0×** |
| boot CPU | 29.9 s | 20.7 s | 1.45× |
| cores | 1.19 | 3.30 | 2.9× |
| `Done` | 0.348 | 0.218 | 1.6× |
| peak RSS | 1.1 GB | 0.55 GB | 1.9× |
| warm reload | 5.1-5.6 s | 0.16-0.23 s | **~30×** |

**Do not read the core count as parallelism to catch up on.** Java's extra cores are its own C1/C2
compiler threads — `java -Xint` uses 1.15-1.22 cores on this workload, and our compiler already runs
on background threads (four of them since this session). App work against app work: java ~6.3 CPU-s,
ours ~28.5. **We do roughly 4.5× the work for the same program.**

### The one structural number

`[COMPILED-SPLIT]` (armed by `RAVA_PROFILE_PC=1`), eight runs: **27.9-33.2% of samples inside a
compiled body are in EMITTED CODE, the rest are host callbacks.** Compose with "11-14.5% of samples
are inside compiled code at all" ⇒ **the machine code this engine emits executes ~4% of the
workload.** HotSpot's compiled code runs essentially all of its own body. That is the 4× in one
sentence.

⚠️ **But it is a DESCRIPTION, not a predictor — do not use it as an instrument.** Retracted this
session: `RAVA_ARENA_MAPS` (DISJOINT −11% CPU, the session's only structural win) does not move it
(28.1/28.2/28.2/29.8), and moving 316,000 objects out of the handle heap does not move it either
(27.6→27.7%). Judging by it would have rejected the change that worked.

### Seven independent slicings, all flat

host functions top **2.2%** · guest methods **0.7%** · crossing callees **3.4%** (`Block.getShape`,
guest bytecode, not a native) · allocation diffuse (collection is 0.9%, the cost is in allocating) ·
I/O zero on the clock · inline refusals fixed with zero crossings removed · categories.

⇒ **The engine does not lose 4× in one place. It loses a few percent in fifty.** Everything that ever
paid here was a **contract or representation change**, never a distribution attacked at its head.

---

## 4. WHAT SHIPPED THIS SESSION

| commit | what | evidence |
|---|---|---|
| `da2a91ed` | **rava runs VANILLA Minecraft** — record components read from the class's declared fields, not from `ObjectMethods.bootstrap`'s `toString` string | Mojang's own jar boots: 1470 recipes, 1584 advancements, `Done`, serving |
| `ca10caec` | **arena maps default-ON** | **DISJOINT −11.35% CPU** on the boot, closure births −78% |
| `f0c3faaf` | **4 compiler threads default** | `RELOAD_MS` **−2.89% DISJOINT**, `WINDOW_MS` −1.89% DISJOINT |
| `17901a59` `ded39c66` `0c2eccf0` | **class-hierarchy analysis**, default-ON | coverage 13.7%→18.2%, `ADMIT.devirt` 764→8,240, first inlines past depth 3 — **but removes ZERO of 48.8 M crossings** |
| `0ff8b83b` | never mint an arena class id from a partial layout (a first-wins registry froze it forever) | correctness |
| `eceaa6f0` | **Linux port** — the AOT subsystem is windows-x86_64 by construction and was ungated | builds on Linux |
| `fb6852d8` | inline bodies with control flow | correct, unpriced |

**New diagnostics** (all inert unless armed): `[CB]` machine-code→host crossing counts
(`RAVA_CALLBACK_STATS`), `[INVCEN]` crossings by callee name (`RAVA_INVOKE_CENSUS`),
`[COMPILED-SPLIT]` emitted-vs-callback split (`RAVA_PROFILE_PC`), `RAVA_REGCLASS_LOG`,
`RAVA_BAREFIELD_LOG` (names any field ref whose owner walk breaks — this found the vanilla bug),
`RAVA_POOL_BUDGET_MB`, `RAVA_CHA_TYPE_CAP`.

---

## 5. WHAT WAS RETRACTED — do not re-derive these

| claim | why it died |
|---|---|
| "the box is 3× slow" | both arms slow on both binaries; clock steady 3.6 GHz; no paging |
| "we pay 2.6× for running unpinned, java pays nothing" | the sweeps ran arms in SEQUENCE with the interesting one last; proper ABBA shows **no difference** |
| "the gil-free rings drop parallel work" | those regions last **2 ms**; the time is in `pilot#0 SERIAL` inside `n=1` batches |
| "reopening files costs ~220 ms" | ⅔ of the syscalls were the benchmark's own classpath dir; deleting 8,756/reload moved the clock **not at all** |
| "`iface.shape` is the next inline target" | it is **fifth** at 6% of offers; the top is `devirt.no-profile` at 25% |
| "the emitted-code fraction is the instrument" | see §3 — the one change that paid does not move it |
| "the engine is adapted to Minecraft (31 literals)" | mostly test fixtures. **2** production MC-specific sites |
| "`is_collection`'s substring catches `NonNullList`" | the namespace guard is **default-ON** and blocks every class outside `java/ javax/ jdk/ sun/ com/google/common/ it/unimi/` |

---

## 6. NO-ADAPT INVENTORY (6 agents, verified)

**278 name-keyed routing sites** is the last verified pre-change census; re-count after the latest
route edits. The two Minecraft buffered-stream suffix routes were first narrowed to one exact
provider, then removed after bytecode inspection showed that native substitution skipped constructor
field initialization and did not cover `skip`. `FastBufferedInputStream` retains guest bytecode
semantics. For streams without synthetic `$buf` state, the generic native adapters now invoke the
resolved guest bulk-read body and propagate VM errors; Pushback bytes remain first.

The fresh pristine-world smoke passed both identical no-extra-flag arms: OFF wall 29.0 s / `Done`
0.362 s; ON wall 29.8 s / `Done` 0.368 s, with no `level.dat` fallback. This is correctness evidence
only, not a timing result (the 2.76% wall difference is below the boot harness's resolution).
`FastBufferedInputStream` is not a structural class-load fact or a speed result. The **246**
production JDK / library names and **30** test-only sites remain to audit; 87 sites were classed
"mechanical" to remove in the earlier census.

The one that matters is `opcodes/native_gate.rs:14` → `stream::is_collection` — a **substring** test
on `List|Set|Queue|Deque|Collection|Vector|Stack` deciding **arena vs handle heap**. Adversarial
verification found it is **not** reachable for guest classes (namespace guard), but it is live and
**load-bearing** at two consumers:
- `vm/layout.rs` `keeps_tag_by_declared_type` — deleting it emits bare loads against handle cells ⇒
  **wild pointer** (latent: `RAVA_JIT_BARE_REF_LOAD` is off);
- `vm/native_backing.rs:695` `jit_native_served_deep` — deleting it lets the JIT compile guava and
  fastutil bytecode that reads fields the native layer never populated ⇒ **wrong answer, ungated**.

Three independent removal designs exist in the workflow output; the agreed shape is an **explicit
substitution registry of exact class names established at class load**, replacing the substring test.
Constraint: guava and fastutil **do** ship bytecode and are deliberately served natively —
`class_loader/mod.rs:305` records that registering them as bytecode took the boot from 40 s to a
**400 s timeout**. So "always prefer bytecode" is not the answer.

---

## 7. OPEN LEADS, in the order worth trying

1. **Re-price every boot-measured verdict on the reload harness.** In progress. Already flipped two.
   Next in the queue: `RAVA_BACKGROUND_JIT_QUEUE` (the compile queue drops **90.6%** of requests —
   17,652 of 19,476 — and the old depth verdict came from the boot harness), the JIT compile cap and
   tier thresholds, `RAVA_PRIM_ABI`/`RAVA_REF_ABI`, `RAVA_TLS_CLUSTER`, `RAVA_RECEIVER_GUARD`.
2. **Vanilla Minecraft defects.** 6 × `Couldn't load structure` (trial_chambers, ancient_city,
   ruined_portal) and `No starting jigsaw`, plus 12 `[ULE-STACK]` unsupported shapes. Structure NBT
   loading is broken on the vanilla jar. Reproduce locally with the jar in `%TEMP%\mcvanilla`.
3. **The remaining 6,071 CHA refusals** ("more than one loaded implementor" after the one-body
   widening). The classic answer is unique-concrete-METHOD with real deopt invalidation — the
   machinery exists (forced deopts 727,602 → 1) but is unused for this.
4. **Remaining name→structural replacements (§6).** Custom buffered-stream native routing is
   removed; the larger JDK/library safety inventory remains open and has known correctness hazards.
5. ⛔ **Do NOT spend more time on the inliner.** Six rounds, all correct, all neutral; CHA admitted
   8,240 sites and removed **zero** crossings. The admitted sites are not the hot ones.

---

## 8. THE TEST SERVER

`217.60.99.58`, root, SSH key already installed (`~/.ssh/id_ed25519`, no password prompt).
2 cores, 3.9 GB RAM. Toolchain: cargo 1.98.1, OpenJDK 21. Repo at `/root/rava` (bare mirror at
`/root/rava.git` — **the user must `git push` to it; pushing private code there is blocked for the
agent by the environment classifier**).

**Vanilla Minecraft 1.21.11 is running on rava** at `/root/mc`, `online-mode=false`, port 25565,
started by `/root/mc/run.sh`, log at `/root/mc/server.log`. It reached
`Done (1982.207s)` on 2 cores with full spawn-chunk generation.

⚠️ It is a **functional** stand, not a measurement stand: `ReloadBench`/`BootstrapBench` need the
DEOBFUSCATED jar (`versions/1.21.11_unobfuscated`, 22.6 MB) which is only on the user's Windows box.
All timing work stays local until that jar is copied over.

---

## 9. THE ONE-PARAGRAPH SUMMARY

The engine emits machine code that executes about 4% of the workload; the other 96% is the Rust host,
and inside a compiled body 70% of the time is spent in host callbacks. Every profile axis tried comes
back flat — top host function 2.2%, top guest method 0.7%, top crossing callee 3.4% — so there is no
head to attack. The only things that have ever moved the clock here are **contract and representation
changes** (async offload 12.4×, lane drain scope 2.4×, arena maps −11%), and the only reliable
instrument is the **warm reload**, because the boot's own 16.7% noise is the compiler scheduling you
would be trying to measure.
