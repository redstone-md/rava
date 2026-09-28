# First-entry compilation work, 2026-09-05

These experiments preserve the full boot objective. None establishes startup
below five seconds or full no-adapt compliance.

## Policy controls

Binary SHA-256:
`18985DD93BF325683874C8084A59E0FEE378A1B95799DD30DB65B562E3F4BD1A`.
All runs use first-entry JIT, standing memory/feature flags, independent copies
of the pristine world, port 25599 and affinity 0x0FFF. No profiling or snapshot
flags are present. Each reaches the full Done line.

| Compile scheduling | Inline budget | Wall | CPU | Evidence suffix |
| --- | ---: | ---: | ---: | --- |
| Background | 0 | 45,368 ms | 51.09375 s | `c2d28cd505954e59b757735f3d04f43b` |
| Background | default 6000 | 44,902 ms | 52.62500 s | `b58e097e2ade43218e83bd2678908a45` |
| Synchronous | default 6000 | 50,797 ms | 49.12500 s | `528a91252f2944f982b44cccd666b920` |

Directories are `%TEMP%/rava-product-boot-<suffix>`. These are exploratory
controls, not repeated estimates of small effects; host quiet counters were
unavailable. Neither removing tree inlining nor waiting synchronously for every
first-entry compilation supplies the required startup performance. No default
policy changed.

## Compiler scope trace

The existing `RAVA_TRACE_COMPILE_PHASES` diagnostic now assigns a unique ID to
each `compile_with_static_intrinsics` driver invocation. Phase records carry
elapsed microseconds; a drop guard emits an end record on normal early returns
as well as completed code generation. Process termination cannot guarantee a
drop record. Disabled tracing does not collect timestamps or allocate IDs.

The trace records elapsed intervals, including waits and instrumentation. It
does not measure thread CPU, and excludes preparation before driver entry and
other compiler entry points. Phase differences also include the preceding log
write. They are not isolated internal-function timings.

Release build SHA-256:
`9DC9DCAC3774CE52D10B1E97F2E89450AA24228AE4513E82E4F10871E26D7CC0`.
Synchronous first-entry trace evidence:
`%TEMP%/rava-product-boot-e6ed25a5ffea453a9b4c6b35be64d24f`.

The full instrumented boot reached Done with 1470 recipes and 1584 advancements.
Its wall was 69,788 ms and CPU 61.4375 s. This timing is not an ordinary startup
result and must not replace the approximately 30-second production measurements.

`bench/analyze_compile_trace.ps1 -Directory <evidence-directory>` parses the
records, rejects duplicate end IDs, counts unmatched phase IDs, and computes
the union of completed driver intervals inside the launch-to-Done window.
It also emits `compile-times.json` with the longest scopes and phase sums.

- 4,186 completed scopes before Done, all on one thread; no unmatched phase IDs.
- Interval union and elapsed sum both 20,114,998 microseconds: no overlap here.
- For scopes reaching all three markers, phase elapsed sums are 525,594 us before
  `body.done`, 4,689,090 us to `finalize.done`, and 10,800,199 us to `codegen.done`.
- The longest completed scope is 211,888 us for a 3327-byte body.

A synthetic parser check with overlapping intervals verifies a 7000 us union
against a 9000 us sum, and validates phase differences. Evidence:
`%TEMP%/rava-compile-trace-parser-b33b3fa5ab154a94bdc22c4978968d92`.

These measurements distinguish driver work from the whole boot. They do not
justify subtracting 20.115 s from the observed wall to predict AOT performance:
logging can perturb both the scopes and their surroundings, preparation is
excluded, and an offline compiler may change the generated execution itself.
The next architectural decision must account for preparation and runtime work
outside this driver as well as native code generation.

First-entry synchronous CompSem matches all 48 normalized Java reference lines
with tracing off and on (two existing declared divergences). The trace contains
4,168 completed scopes; all three IR-budget refusal records have matching end
records. Preserved output and checks:
`%TEMP%/rava-compile-duration-validation-02a910a721134bbfb7b4fe3b6d38a416`.

## Buffered first-entry follow-up

After fixing synchronous per-phase stderr distortion, the same current release
CB4DFE4B was run with first-entry compilation and buffered compiler tracing.
Evidence: `%TEMP%/rava-product-boot-ac3e0574fadc40a187ac3e13208ccdb8`.
Full Done, 1470 recipes and 1584 advancements were observed. Instrumented wall
was 39.398 seconds, CPU 46.703125 seconds, peak working set 1,571,946,496 bytes.
The parser accepted 4142 complete scopes with zero unmatched or outside-window
records. Their interval union was 3.244153 seconds and summed elapsed 8.531077
seconds; phase sums were 7.222070 seconds codegen, 0.789640 seconds finalize,
and 0.481408 seconds before body completion. These are elapsed intervals,
not CPU time or an additive startup breakdown.

The final recorded driver ended 10.852 seconds after launch. Done was another
28.557 seconds later. Consequently the instrumented trace does not support
blaming the entire first-entry slowdown on code generation. Preparation before
driver entry remains excluded. This background run also differs in scheduling
from the older synchronous 20.115-second trace above; do not attribute the whole
difference to the logging fix.

Correction: the initial source audit incorrectly ruled out the method cap by
reading only the interpreter's 16384 default. The actual copied `rava.toml`
sets `jit.max_methods=4096` and `jit.threshold=512`. The CLI resolves these and
publishes them into environment variables before creating the VM, overriding
its defaults. Thus the cap remains a primary explanation requiring a control.
`RAVA_JIT_TIER0=1` does bypass `callback_heavy`, but does not override the
explicit threshold. `collect_compiled_callee_sites` therefore cannot cause
that classifier's refusal in this run.

One remaining hypothesis is early plan quality: `build_new_arena` only admits
already initialized classes because its emitted allocation skips class-init
callbacks. First-entry compilation can miss that opportunity permanently.
The existing late-new machine-code slot can recover this opportunity after
runtime resolution, so compile-time loss alone is not a measured cause.
Do not remove the class-init guard. The broad
`RAVA_NO_ARENA_INLINE` control is unsuitable: it also changes field emission and
has documented full-boot correctness failures. Any experiment must isolate the
allocation plan while preserving runtime representation and initialization.

Uninstrumented current-release first-entry control reached Done in 38.986
seconds (CPU 46.875 seconds, peak working set 1,422,409,728 bytes):
`%TEMP%/rava-product-boot-6e3b0a2114784a85bd29175e8dbcbcd9`.
The mode's roughly 39-second runtime is therefore also present with compiler
tracing disabled. This one control is not an interleaved estimate of a small
change against the historical 44.9-second first-entry result. Current ordinary
JIT remains approximately 27 seconds and is not certified no-adapt. No production
policy changed; the next experiment must measure plan quality or runtime work,
not merely remove more compiler logging.

## Effective method cap: verified correction

The standing fixture and CLI, not the VM constructor defaults, determine the
actual settings. `D:/code/rava/rava.toml` sets `jit.max_methods=4096` and
`jit.threshold=512`; `Config::apply` publishes both into RAVA environment values.
An explicit `RAVA_JIT_CAP` overrides the file. Prior documentation dismissing cap
starvation from the 16384 Rust fallback was incorrect and has been corrected.

Same executable SHA-256
`078DE86F2C7E04D93B50EB1AE0EA980494602631B30DC1795F7878CDF9452D20`, first-entry
JIT, fresh pristine-world copies, standing memory limits, no snapshots or
prepared archive, no instrumentation:

| Cap | Wall | CPU | Product-boot suffix |
| --- | ---: | ---: | --- |
| 16384 | 26.020 s | 44.96875 s | 00348e60b51541459b8322917c179373 |
| 4096 | 39.273 s | 46.453125 s | 1402f8ca102346638066b622c8a78307 |
| 16384 | 25.680 s | 46.265625 s | f7edc0cbb1f94a0397dfef8f5cdc1520 |

Each run reached full Done. Quiet counters were unavailable. The repeated large
wall change supports the configured cap as a cause of the first-entry regression;
this is recovery to the original 27-30-second range, not the below-five-second
goal. CPU stays about 45-46 seconds, suggesting additional overlap; these totals
alone cannot quantify compiler versus guest execution or prove whole-runtime
no-adapt compliance. Full semantic validation with the higher cap remains pending.
Future first-entry experiments must explicitly set and record cap 16384 rather
than accidentally inheriting 4096. The user's source fixture was not edited.

Experiment scripts and logs: directory referenced by
`%TEMP%/rava-jit-bail-compare-current.txt`. `boot-cap.ps1` sets the explicit cap.
Its preceding diagnostic runs (first entry 41.949 s, ordinary 27.005 s) are not
performance controls: `RAVA_DBG_CBAIL` and `RAVA_TRACE_PLANS` were enabled.
First entry produced 37 CBAIL records (34 exceptions, 3 emit_body); ordinary
produced 11 (10 exceptions, 1 emit_body). No `resolve_fields` or `PLANS-NONE`
records were observed. This limited trace does not cover all rejection sites,
so it does not prove all failures permanent. The known emit-body logger also
labels IR-budget refusals as emit_body; do not infer an opcode failure from it.

## High-cap semantic validation and current compiler trace

All 41 differential probes match Java -Xint with explicit first-entry JIT,
cap 16384, standing 4 GiB memory bounds and the suite's declared divergences.
Executable remains 078DE86F. Evidence:
`%TEMP%/rava-first-entry-cap-validation-16d7550157f94c57bf8c2400128b5214`.
This supersedes the earlier pending high-cap semantic check. It does not
certify the full product hash suite or all no-adapt policies.

A fresh buffered compile trace reached full Done in 26.301 seconds
(instrumented, CPU 46.0625 s):
`%TEMP%/rava-product-boot-bb863d6315154f6dafc5aa4c6afd8a19`.
Verbose CLI output confirms `cap 16384, threshold 512` after configuration
resolution. First-entry overrides the invocation-count gate; the recorded
threshold remains relevant to understanding other ordinary-mode measurements.

Parser: 8421 completed driver scopes inside launch-to-Done, 12 outside the
window excluded, zero unmatched phase IDs. Union 7.170083 seconds, summed
elapsed 21.155032 seconds. Phase sums: codegen 17.813588 seconds, finalize
2.152301 seconds, before-body 1.141499 seconds. These overlap five threads,
exclude planning and remain instrumented elapsed values, not CPU attribution
or removable wall. The earlier 4142-scope capped trace is not representative
of this configuration.

Because the high-cap run compiles substantially more methods than ordinary JIT,
a bounded control of the existing `RAVA_JIT_REGALLOC_FAST` flag is now relevant.
No default changes: its compile-time saving must outweigh additional machine-code
spills, and the old ordinary-mode optimization-level nulls still stand.

The exploratory allocator control did not improve startup:

| Register allocator | Wall | CPU | Product-boot suffix |
| --- | ---: | ---: | --- |
| single_pass | 27.706 s | 42.703125 s | 1d83b01cc4f64b48802ab6e121d57f55 |
| default | 25.555 s | 45.65625 s | 686a2ee06fc246388dac3fdab53bfa5e |

Both use the same executable, first entry and cap 16384, no instrumentation,
and reached full Done. This single sequential pair does not estimate a small
causal effect, but offers no reason to promote the fast allocator: less total
CPU did not produce less elapsed startup. No new runtime default changed.
The fast arm has not received full semantic/product certification and is not
an accepted candidate. The standard allocator remains the work configuration.
