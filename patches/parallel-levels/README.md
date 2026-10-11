# parallel-levels

The official rava patch for NeoForge 1.21.1 servers. It ticks the server's levels
(dimensions) at the same time on several threads instead of one after another. Mods'
level-tick event handlers stay on the server thread, because mods share state between
levels.

## Install

```
patches/parallel-levels/build.sh <server dir>           # compiles against the server's jars
mkdir -p <server dir>/rava-patches
cp patches/parallel-levels/parallel-levels.jar <server dir>/rava-patches/
```

The console shows `[rava] patch parallel-levels` when the patch is loaded, and
`[parallel-levels] N levels, M busy: ticking in parallel` the first time levels tick in
parallel. If you start with
`-Drava.parallelLevels=false`, the patch stays installed but the levels tick one after
another.

## What it does

`MinecraftServer.tickChildren` handles each level in turn: it fires `LevelTickEvent.Pre`,
calls `ServerLevel.tick`, then fires `LevelTickEvent.Post`. The patch redirects those three
calls so each level is only queued. At the first profiler section switch after the loop, the
queued levels run:

1. The server thread fires every level's `LevelTickEvent.Pre`, in the original order.
2. The levels tick in parallel. The server thread and `min(levels, cores) - 1` workers take
   levels from a shared counter, the slowest level of the last tick first.
3. The server thread fires every level's `LevelTickEvent.Post`, in the original order.

Rules while the levels tick in parallel:

- **Ownership.** While a worker ticks a level, it is that level's thread: `Level.thread` and
  `ServerChunkCache.mainThread` point to the worker, so chunk and block-entity access takes
  the normal on-thread paths. Both are restored afterwards.
- **Events.** Events fired inside a level's tick (spawning checks, entity joins, chunk
  loads...) are dispatched one at a time under one lock (`EventBus.post` is redirected).
  Mods' handlers therefore never run concurrently with each other.
- **Cross-level requests.** A chunk request to another level goes to that level's task queue.
  Once out of levels, the server thread serves the queues of finished levels until the
  workers are done.
- **Portals.** An entity going through a portal from a worker is handled on the server thread
  after the tick, because going through reaches into the destination level.
- **Failures.** A crash in any level stops the server with the usual
  "Exception ticking world" report.
- **When levels run in turn.** Levels run one after another, exactly as without the patch,
  unless at least two levels took over 0.1 ms in their last tick: an idle level ticks in a
  few microseconds, less than it costs to hand it to another thread
  (`-Drava.parallelLevels.busyMicros=N` changes the limit). They also run in turn
  while the profiler is active (`/debug start`, JFR).

Why only the level tick: on All The Mods 10 (29 levels, no players) a tick's level work is
about 440 µs of `LevelTickEvent` handlers and about 90 µs of `ServerLevel.tick`. A first
version also ran the handlers in parallel. In the first sprint, In Control's shared
`HashMap` threw `ConcurrentModificationException`, because handlers of many mods share state
between levels. Those handlers have to stay on one thread.

## Risks

Events are serialized, but block entities and entities of mods still tick in parallel with
other levels' block entities and entities. If one of them shares unsynchronized state with
code in another level, for example a network spanning dimensions, it can fail. Each level
still ticks on one thread at a time, but the order of effects *between* levels within one
tick is no longer fixed. An event handler that waits on a chunk of a level another worker is
still ticking, while that worker waits for the event lock, deadlocks the tick (the server's
watchdog then stops the server). Test a modpack with the patch before you rely on it. If
something goes wrong, remove the jar or start with `-Drava.parallelLevels=false`.

Results with the patch installed measure the patch as well as the VM. They are reported
apart from rava-versus-HotSpot comparisons (`docs/progress.md`).
