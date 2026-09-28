# How to measure a change in this tree

Written after a session that closed eight hypotheses as "zero" on an instrument
that could not have seen any of them, and then found one of the eight to be
worth −19.9%. Everything here is a consequence of numbers in
`docs/boot-speed-ledger.md` rows 244-257.

## The one-paragraph version

Price a change on **fixed work** (a gate), with a **paired, alternating,
hash-verified** A/B, comparing the benchmark's **own timer** and only **within a
batch**. Convert to boot seconds with the boot's own census. Take the **sum** of
several changes back to the boot only once it clears ~5%. Never conclude from a
profile share, a callback count, or a single boot.

## What each instrument can and cannot resolve

| instrument | resolves | why |
|---|---:|---|
| paired boot A/B, wall (`wt_boot_ab.ps1`, ABBA) | ~±4% | other processes on the box lengthen wall |
| paired boot A/B, `Done` | **~±5%, and NOT below** | measured against a byte-identical copy of the same binary: ±1.4% mean, 9.7% spread |
| paired boot A/B, CPU-seconds | ~7-15% | the JIT compiler threads and the ring's workers do a DIFFERENT AMOUNT of work run to run |
| paired gate A/B, process wall (`gate_ab.ps1`) | ~5% | 31 s of a 37 s ChunkFill run is JVM startup |
| paired gate A/B, the gate's OWN timer | **~±0.4% at n=6; ~±2.7% at n=1** | fixed work, and the hash proves it was the same work — self-A/B calibrated, 12 known-zero pairs |

The last row is the only one that can price the changes worth making. The call
protocol is **225 ns over 148,869,385 dynamic calls per boot**, so a saving of
10 ns/call is 1.6% of wall — invisible to every row above the last.

## The arithmetic that converts a gate result into boot seconds

    boot        46.2 s wall · 95.6 CPU-s · 2.07 cores average
    compiled    19.7% of live samples are inside compiled guest code
    calls       148,869,385 dynamic calls out of compiled code, 27.0% fall to the Rust host

So a change confined to compiled code that is worth X% there is worth about
`0.197 · X` of the boot. **−20% on a gate is ~4% of a boot, which the boot
cannot resolve** — this is why the gate is the instrument and the boot is the
confirmation, never the other way round.

## Rules that were each paid for

1. **Compare the benchmark's own timer, not the process clock.** The first
   reading of the three-change stack said −1.50% on ChunkFill process wall and
   −20.68% on `FILL_MS`. The process clock was measuring the harness.
2. **Only WITHIN-batch pairs are comparable.** The same binary measured
   `FILL_MS` 6,207 and 5,934 in two batches — 4.6% of drift. A number read
   against another batch's number threw away the alternation that exists to
   cancel exactly that, and produced a "+6.2% loss" that had to be withdrawn.
3. **Verify the hash on EVERY run.** A speed A/B whose arms compute different
   answers is not a speed A/B. `gate_ab.ps1` withholds the verdict on a
   mismatch rather than printing a number.
4. **A win measured on one gate is a win on that gate.** The dispatch table is
   −19.9% on ChunkFill (single-column, main-thread worldgen) and +1.9% on
   RingScan (the parallel ring), because a worker's megamorphic answers already
   come from the worker-only freeze table. Say which path you priced.
5. **Decompose a stack before shipping it.** A stack measured −20.7%; one member
   was −24.6% and the other was not separable from zero. Shipping it whole would
   have banked both.
6. **A change that is not measurably positive does not get to add a soundness
   obligation.** Two changes measured the same way; the one that demanded a
   standing invariant was reverted and the one that demanded nothing was kept.
7. **Attribution does not predict wall.** The PC profile billed 9.95% to
   `jit_direct_invoke` and removing work there changed nothing; the bridge census
   billed 17.3% to `Objects.equals` and removing 338,112 interpreter re-entries
   changed nothing; `memchr` was 3.23% and the six sites that obviously produced
   it were worth zero. A profile row names a symptom that several callers share.
8. **A sweep needs a control.** A list of flags that all report "neutral" is
   indistinguishable from an instrument that cannot hear. Put something with a
   known-large effect first (`RAVA_ARENA_MAPS=1` is +48% on a boot).
9. **Measure the PRODUCT before believing the session.** A whole session of gate
   work was banked without once building the shipped binary. The rebuilt
   boot-trained `pgo-fat` was **+6.5% slower than the previous product** — 4/4
   pairs, zero overlap — and reproduced larger in plain `release`, so it was the
   source and not the build. Nothing in the gate record said so.
10. **The gate is blind to the paths it does not run, and "measures zero" is a
    reason to REVERT.** This is the converse of the `0.197 ×` rule above. A
    commit that measured zero on ChunkFill cost **+9.59% of Done** on a boot,
    because class loading and datapack decode are collection-and-string heavy
    with class diversity the gate never sees. A change with no measured benefit
    is pure risk: it was kept on "measures zero, so it is free", and the ledger
    had already recorded that its stated justification was wrong.
11. **Count the population before optimising it.** A census (`--features
    contract-stat`) refuted three plausible, mechanism-backed theories in one
    boot each — `key_str` builds 1.8 MB of key text in a whole boot and cannot be
    11% of anything — and found two real wastes in the same pass. A count is not
    an opinion, and it is one boot.
12. **Some of the price is the right answer.** After both real wastes were fixed,
    most of the correctness cost remained, and it is intrinsic: correct
    collections are bigger, and mutators that now throw send guest code down its
    catch and copy paths. The engine was faster because it answered wrongly. Do
    not go looking for that time again.

13. **The BUILD needs a control too, and it passes.** Rule 8 asks for a control in a
    flag sweep and said nothing about binaries, so every attribution made by
    comparing two separately-built executables rested on an unmeasured assumption.
    Measured: the same source built twice, differing only by 18 KB of dead code in
    a hot module, is **wall −0.02%, cpu −0.34%, done −0.19% over 5 pairs**. Code
    layout is not a confounder at this tree's scale, and a bisect step that reads
    several percent is reading the change, not the compiler.
14. **Three failed explanations beat one confident wrong one.** A +17.85% regression
    was explained as guest re-reads (refuted: the function runs under 5,000 times a
    boot), then as code layout (refuted by rule 13's control). It is recorded as
    UNEXPLAINED. An attribution to a commit is evidence; a story about the
    mechanism is a separate claim that needs its own.

15. **A same-binary FLAG A/B outranks a two-binary A/B, and the gap is not small.**
    A change measured **+17.85% of `Done`** by two independent paired two-binary
    A/Bs (n=4 and n=3, different baselines, agreeing) measured **−0.05%** when the
    same change was put behind a flag and both arms ran the SAME executable. The
    flag design wins by construction: no separate compile, no layout difference,
    no build-time machine state — the arms differ in nothing but the executed
    path. Alternation and pairing protect against DRIFT; they do not protect
    against whatever differs between two separately-produced binaries. Rule 13's
    dead-code control was too weak to expose this, because dead code is never
    reached and cannot perturb what reachable code does.

    **Order of trust, measured rather than assumed:**
    same-binary flag A/B  >  two-binary A/B on identical source  >  two-binary
    A/B across a commit. Express a change as a flag whenever it can be, and treat
    a large two-binary reading as a HYPOTHESIS until a flag confirms it.
16. **Check the knob is wired before believing the verdict.** A flag A/B reported a
    flawless, quiet, perfectly-paired `+0.07%` for a flag that did not exist in the
    binary under test — the harness defaulted to `target\pgo-fat
ava.exe` and the
    code had only been built into `target
elease
ava.exe`. Eight boots measured
    a thing against itself. `bench/assert_flag.ps1` now refuses (exit 5) unless the
    flag name appears literally in the executable, and it fails CLOSED.

17. **RUN THE INSTRUMENT AGAINST ITSELF BEFORE BELIEVING ONE READING FROM IT.**
    A whole session priced changes with the boot A/B and never asked what it says
    when the answer is known to be zero. Against a byte-identical COPY of the same
    binary (md5 verified, n=6) it said **`Done` +3.56%, with the second arm losing
    5 pairs out of 6** — a POSITIONAL bias, plus a 20% full spread that supplies
    fat-tail pairs on demand. Pairing cancels drift BETWEEN pairs; it does nothing
    about a penalty that lands on whichever run goes SECOND.

    Fixed by running **ABBA** — swap the arm order every other pair — which put the
    win rates back to coin flips (3/6, 4/6, 4/6) and halved the `Done` spread to
    9.7%. A self-A/B costs one batch and is the cheapest calibration available.

18. **THE GATE PASSES THE SAME TEST, AND NOW ITS RESOLUTION IS A MEASUREMENT.** Rule 17
    was paid for on the boot harness; applying it to `gate_ab.ps1` — the instrument that
    priced every shipped win in the ledger — was the obvious next question and it had
    never been asked. Against a byte-identical copy of the same binary it read **−0.06%
    (3/6 pairs)** under its original ABAB order and **+0.37% (2/6)** after the switch to
    ABBA: two orders differing by less than one pair's noise, so unlike the boot harness
    there was **no positional bias here to remove**. The ABBA swap is kept for symmetry
    by construction and one convention across the tree, not as a correction.

    Pooling the two batches gives twelve paired readings of a known zero — paired mean
    **+0.18%**, 5/12 wins, per-pair range **−2.13% to +2.70%**:

    > `FILL_MS` resolves **~±0.4% at n=6** and **~±2.7% at n=1**.

    The "~1-2%" this table used to claim was an *n=6* figure quoted as if it were a
    per-reading one. **A two-percent verdict from one or two pairs is noise however
    cleanly it prints** — and the fix is pairs, not precision: pair 5 of the first batch
    had both arms rise together (5218/5107) on a slow moment of the box, which is a 14%
    within-side spread that the pairing absorbed to −2.13%.

    ⇒ The standing claim that the gate is the instrument and the boot is the confirmation
    is no longer an argument from fixed work. It is two calibrations: the boot harness
    was biased and the gate was not.

18. **Know what your number MEASURES before you attribute anything to it.** `Done`
    was quoted all session as "the boot". It is the guest's own clock, it starts at
    *Starting minecraft server*, and it covers the **last ~15 s of a ~46 s run**.
    The rest — startup, `Bootstrap`, registries, datapacks, recipes — was invisible
    to both numbers the boot harness printed, and the growing `wall − Done` gap got
    explained three times as "the world save", a phase that never runs: the harness
    KILLS the process the moment `Done (` appears. `wt_boot_ab.ps1` now prints a
    phase map per run so the block that moved has a name.

19. **A check whose success you cannot SEE in the output is not a check.** The guard
    written to refuse a flag A/B on a binary that lacks the flag failed silently
    TWICE in one session: first it threw on a missing .NET method and printed "is
    present" without having looked, then its dot-source line was mangled to
    `$PSScriptRoot<BEL>ssert_flag.ps1` and it never loaded at all, while the batch
    ran on regardless. Both times the output looked exactly like a passing run.
    Guards must print on SUCCESS — `flag check: 'X' is present in ...` — so the
    absence of that line is the alarm, and they must fail CLOSED.

20. **BOOT THE SERVER BEFORE YOU COMMIT A BATCH, not after.** Thirty-odd correctness
    fixes went in with ten differential probes green and 417 tests green, and the
    boot was dead before its first log line — three separate regressions, one of
    them a match-arm ordering slip that made `getFirst` throw on every call. Every
    probe exercised the case the new code was written FOR; none exercised the case
    it had to PRESERVE. **A probe proves the behaviour you added, not the behaviour
    you kept.** The boot is the only instrument that runs the paths nobody thought
    to write a probe for.
21. **A wrong answer is not a fast path.** The ring published a thread-local box
    index across threads and returned 0 for every boxed numeric; the fix sends those
    tasks for a serial re-run, which should have cost parallelism. Measured, it is
    **3-6% FASTER than the bug** — downstream code took wrong branches on wrong
    values, and doing the right work serially beat doing the wrong work in parallel.

22. **A win rate is consistency INSIDE a batch, not reproducibility ACROSS batches.**
    Two ABBA batches of the same two binaries an hour apart returned `cpu` −1.18%
    at **5/6** and +3.42% at **0/6** — confident, unanimous, opposite. Rule 2 says
    only within-batch pairs are comparable; this is the sharper form: a within-batch
    VERDICT can flip between batches once the effect is smaller than batch-to-batch
    drift. Before trusting a small result, run the batch TWICE. `wall` survived that
    test here (five batches, +3.3% to +6.3%, every one 0/6); `cpu` did not.

23. **A frequency tally names WHAT. Only a stack names WHO.** A census counted
    4,512,560 known-absent class probes a boot and tallied the NAMES, which read as
    `ArrayList`'s super and interface closure. Two experiments were then spent making
    a probe cheaper, and both lost. Sampling a BACKTRACE — one capture per 10,000
    probes, ~450 a boot, cheap enough not to distort — put **353 of 450 samples in one
    chain** and named the caller in a single run. The tally had been read as an
    attribution, and it never was one: the names were the closure of ONE repeated walk,
    not a population of independent callers. When a count is large and the fix keeps
    missing, stop pricing the operation and ask which code path performs it.

24. **A revert has to be READ IN THE DIFF, not inferred from a green build.** A commit
    claiming to remove a thread-local cache spliced its new comment ABOVE the live code
    instead of replacing it, so the cache shipped, still default-on, under a comment
    saying it had been removed. A boot and 417 tests passed — of course they did: the
    cache was never incorrect, only unproven. **A diff that only ADDS lines looks clean
    in review, and no test can be evidence for a deletion that did not happen.** Grep
    for the thing you removed; if it is still there, so is the code.

25. **A profile says WHERE, a census says WHAT, and a fast path that serves nothing needs
    a third instrument: WHY IT REFUSED.** The ring's lock cost was found by a PC profile
    (`lock_contended` 8.49%), narrowed by a file:line attribution to one gate, and only
    became actionable when a per-MEMBER tally said the gate's entire traffic was
    `getDouble` — a read. Then the fix served **zero of eighty million calls**, because it
    admitted the class that looked safest and the receivers were 100% the other one. No
    counter in the tree could say which precondition failed; two builds went on guesses.
    An instrumented bail reason answered it in a single run. **Before widening a new fast
    path's admission, make it say why it refused.**

26. **A DIFF THAT STOPS IS A FINDING, AND IT SILENTLY WITHDRAWS EVERYTHING BEHIND IT.**
    bench/MapViewSem.java reported four divergent lines and then nothing: rava's output
    simply ENDED, because `Map.replace` did not exist and the `UnsatisfiedLinkError` killed
    the program. Twenty observables after that line — compute, merge, the unmodifiable
    wrappers, null handling — had never been tested at all, and the summary line ("4
    divergent") gave no hint of it. **Compare LINE COUNTS as well as content: a probe that
    printed fewer lines than the reference did not pass the rest, it never ran them.**

27. **ONE EXAMPLE OF A FORMAT TEACHES THE WRONG RULE.** The JVM's helpful NPE renders
    `Cannot invoke "String.length()"`, from which "strip the java.lang package" is the
    obvious inference — and it is false. `java.lang.Integer`, `java.lang.StringBuilder`,
    `java.lang.Thread` and `java.lang.CharSequence` all keep their package; the real rule is
    a two-name exception list for `String` and `Object`. Seven more shapes were needed to
    find that. **When reproducing a reference implementation's OUTPUT, enumerate the space
    before writing the formatter, not after the first failure.**

## The harnesses

| script | what it is for |
|---|---|
| `gate_ab.ps1` | two binaries (or one binary ± a flag) on fixed work; waits for a quiet box |
| `gate_sweep.ps1` | a list of flags, one paired gate A/B each |
| `gate_pc.ps1` | PC profile of ONE gate — the fixed-work counterpart to `boot_pc.ps1` |
| `wt_boot_ab.ps1` | two binaries over a real boot; reports wall, CPU and Done |
| `flag_ab.ps1` | one flag over a real boot |
| `bisect_boot.ps1` | attribute a boot regression to the commit that carries it |
| `boot_counts.ps1` | collect a COUNT from a boot without the quiet check (a counter does not care about the box) |

`bench/bin/` holds per-commit binaries and is gitignored, as are the `*_ab.txt`
transcripts: they are outputs of a measurement, not inputs to one.
