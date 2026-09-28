# Resolved metadata for direct prepared calls

The flatcall preference experiment did not reduce boot wall or CPU. Avoid
repeating metadata resolution rather than adding another execution tier.

RAVA_AOT_RESOLVED_CACHE is experimental, off by default. The per-VM memo stores
only successful metadata resolutions. Its key contains caller class identity,
constant-pool entry, invocation kind and actual arena receiver class ID (zero
for static calls). Other receiver representations retain ordinary resolution.
The loader epoch invalidates all entries. A new class or previously missing
source therefore cannot reuse a stale hierarchy proof. No failures, call counts,
observed return values or hotness decisions are retained.

The stored target owns an Arc to the class and an index into its immutable
method list. Root raw arguments before either resolution or execution. Release
the memo borrow before entering guest code. All shared runner checks, including
class initialization and native backing, still run on every entry.

Validate alternating receiver implementations, null handling, initialization
order and GC with actual resolution/execution trace counts, then compare full
boots using the same executable and prepared objects with the memo off and on.

58 AOT-related unit tests pass. The release dispatch probe passes with the memo
on and off, including its moving-GC execution. With the memo enabled, Base.step
and Derived.step each execute 1000 times but resolve only twice: before and
after a new class is deliberately loaded halfway through the alternating loop.
This checks actual reuse and epoch invalidation rather than only equal output.
Enabled evidence: `%TEMP%/rava-aot-dispatch-fd0d9ab832374013a7bff9da7acb425c`.
Disabled evidence: `%TEMP%/rava-aot-dispatch-890a25dd963043f6b200134bf9ca857d`.
All 38 semantic probes match Java -Xint with JIT disabled and the memo enabled.
The full prepared classpath contains 30,462 classes, 292,541 compiled methods
and 56,526 refusals. Executable SHA256:
`BC75B0B0BC8A93733A60160C55B9C21CEBA524F9939033A304A917286841462B`.
Executable, matching objects and validation evidence:
`%TEMP%/rava-aot-resolved-server-4d64a84607af478d8ff430532c3cac99`.

## First paired boot comparison

Both runs used the same executable and prepared objects, RAVA_JIT=0, fresh
world copies and no snapshots. The memo flag was the sole requested mode
difference. Quiet-box performance counters were unavailable. No other owned
benchmark or build ran alongside these boots.

| Memo | Process wall to Done | CPU seconds | Boot evidence suffix |
| --- | ---: | ---: | --- |
| Off | 150.519 s | 148.000000 | 3e27ed0c701c452aa0f6cf2cbe40fec8 |
| On | 141.830 s | 139.453125 | eb38064850d343d68ac236c4eca790c2 |

Boot evidence directories are `%TEMP%/rava-product-boot-<suffix>` and contain
the environment, binary identity, fixture hashes and result.json. This first
pair improves wall by 5.77% and CPU by 5.78%; it does not establish variance or
exclude order effects. A reverse-order pair is pending. Keep the memo opt-in.
The full product hash suite remains unverified for this executable, and the
earlier intermittent RingLane crash remains unresolved. The five-second goal
is not achieved.

The reverse-order attempt stopped before guest launch: Windows denied copying
the executable as malware or potentially unwanted software. Defender recorded
`Trojan:Win32/Bearfoos.A!ml`, threat ID 2147731250, for the frozen executable at
12:16:45 Budapest; action succeeded at 12:17:06. The on-arm boot copy was also
detected at 12:16:01, after its recorded Done boundary. Other prior RAVA copies
were detected during this period. Defender activity is an additional timing
confounder, not evidence that the memo improved performance by a stable 5.8%.
The classification has not been independently established as a false positive.
Records are preserved in `defender-detection.json` under the experiment root.
No protection settings, exclusions or quarantined artifacts were changed.
