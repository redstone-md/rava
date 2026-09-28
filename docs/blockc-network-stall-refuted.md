# Block C's "~30 s yggdrasil network timeout" — REFUTED (2026-07-26)

The boot ledger carried block C (the `Environment:` → `Starting minecraft server version`
window, 25-44 s) with the note "includes a ~30 s yggdrasil network timeout". That note is
wrong. There is no network stall anywhere in block C; the window is `WorldLoader.load`
compute, and the log line that suggested a timeout was mis-read.

## What the window actually contains

`bench/BootPhaseC.java` replays `net.minecraft.server.Main` step by step in bytecode order
and times each call (`bench/bpc_prof.log`):

```
STEP new YggdrasilAuthenticationService  <-- 'Environment:' is logged here ms=2
STEP Services.create  <-- 'Failed to request yggdrasil public key' is logged here ms=2
...
STEP WorldLoader.load  <-- datapack + registries + recipes (BLOCK C) ms=28781
```

`Services.create` — the only networking call in the window — takes **2 ms**. The whole gap
is `WorldLoader.load`: registry JSON/DFU decode (~14 s to the `Failed to parse level-type`
line, which the `createDimensions` lambda logs after registries are up), then tags, loot,
advancements, recipes (~15 s to `Loaded 1470 recipes`). Known residents from the ledger —
`VanillaRegistries.createLookup` 6.5 s, of which `Climate$RTree` build ≈5.8 s — live here.
It is our compute gap, not a stall.

## Why it looked like a timeout

The signature was `[ERROR]: Failed to request yggdrasil public key` appearing 18-28 s after
`Environment:`. Three facts break that inference:

1. **The fetch is asynchronous.** authlib 7.0.61 `YggdrasilServicesKeyInfo.get` (bytecode
   verified) submits the fetch to a `ScheduledExecutorService` and returns a **lazy**
   `ServicesKeySet` immediately; `Services.create` measured 2 ms. On failure it reschedules
   with a backoff of `5 * 2^n` **minutes** — nothing in it can produce a ~30 s delay.
2. **The error's timestamp wanders with scheduling, not with any timeout.** Across the
   bench logs the same line prints 0 s after `Environment:` (`full_after.log`), 18-28 s
   after (`fg_on1.log`, `k_on1.log`, `hdr_on1.log`), and even 1 s AFTER
   `Loaded 1470 recipes` (`census_leafpic.log`) — impossible for a synchronous timeout
   inside the window. The fetcher thread simply waits for GIL time behind the
   compute-bound main thread and fails the moment it runs.
3. **The window has no 30 s floor.** Minimum `Environment:` → `Loaded 1470 recipes` across
   the logs is ~24 s (`final_base1.log` 09:15:57 → 09:16:21). A fixed 30 s timeout inside
   the window cannot be beaten by 6 s.

## The engine behavior is already correct (fast, honest failure)

Rava's `HttpURLConnection` is a deliberate non-networking stub
(`crates/rava-interp/src/reflect.rs`, `urlconnection_method`): setup calls are no-ops and
any read throws `IOException("rava: network access is unavailable")` immediately — the
same shape a real offline host produces, which authlib catches and logs.

Standalone repro of the exact JDK call authlib makes (`bench/HttpsFetch.java`,
`URL.openConnection` → 15 s timeouts → `getResponseCode` → `getInputStream`, against the
real `https://api.minecraftservices.com/publickeys`):

| runtime | 1st call | 2nd call | outcome |
|---|---|---|---|
| rava.exe | **31 ms** | 0 ms | `IOException: rava: network access is unavailable` |
| JVM | 5317 ms | 37 ms | `code=200 bytes=3989` |

No hang in either direction; under the JVM the fetch is asynchronous too, so its success
contributes nothing to the JVM's wall either. There is no engine change to make: the
"fetch cannot succeed" path already fails fast and honestly, and making it succeed (a real
host-side TLS client) would buy **zero wall** because nothing in the boot blocks on it.

## Consequence for the map

* Strike "includes a ~30 s yggdrasil network timeout" from the block C row of
  `docs/boot-map-2026-07-26.md`; block C is `WorldLoader.load` compute end to end.
* Block C levers are therefore the already-named compute residents (registry DFU decode,
  `Climate$RTree` comparator build), not I/O. The "single cheapest large win on process
  wall" hypothesis is closed: there is no free 30 s.
* Diagnostic rule worth keeping: an async worker's ERROR line inherits its scheduling
  delay, so under a GIL its timestamp measures scheduler starvation, not the duration of
  the operation it reports. Place log lines on calls with a step-timer replay
  (`bench/BootPhaseC.java`) before believing adjacency.
