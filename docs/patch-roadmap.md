# Patch roadmap

The official patches for NeoForge 1.21.1, area by area. Each patch is measured with a
`bench/scenarios/` load (`bench/tick_sprint.py -c`) on All The Mods 10, before and after. Rules:

- **Vanilla results by default.** A patch computes what vanilla computes, faster. Where it
  can check itself against vanilla, it has a verify mode (`-Drava.<patch>.verify=true`).
- **Gameplay changes are opt-in.** Anything that changes results (update order, skipped
  ticks, merged entities) stays off unless the server owner turns it on.
- **Threads only where the work is independent.** World ticking is ordered: block updates,
  entity moves and redstone see each other's effects within a tick. Moving them to other
  threads changes results, or needs locks that cost more than they save (see
  `parallel-levels`). Threads pay off for work that already runs off the tick (world
  generation, lighting, saving) and for read-only computations whose results are applied in
  order.

Baselines on All The Mods 10 (first sprint after setup; ms per tick, lower is better):

| scenario | sprint | rava | `java` |
|---|---|---|---|
| `tnt` | 600 | 320 (196 with `explosions`) | 217 |
| `mobs` | 1200 | 76.6 | 60.3 |
| `effects` | 1200 | 90.4 | 69.2 |
| `redstone` | 1200 | 31.2 | 23.4 |
| `pistons` | 1200 | 13.6 | 10.8 |

Under these loads rava itself is about 30 % slower than HotSpot, in optimized code (nearly all
samples are `[opt]`), unlike the idle tick where it leads: a VM task of its own, apart from the
patches.

| area | scenario | where the time goes | plan | threads |
|---|---|---|---|---|
| TNT | `tnt` | exposure rays of `Explosion.getSeenPercent`; then mods' `ExplosionEvent` handlers | `explosions` (done: 320 → 196 ms/tick); next: one block lookup per ray step in `Explosion.explode` | the exposure of each entity is read-only, but there are too few rays per explosion to be worth handing out |
| redstone | `redstone` | `GameEventDispatcher.post` 10 %, `RedStoneWireBlock.calculateTargetStrength` 8 %, `getChunk` 7 %, signal lookups 11 %, Aether's block-placement hooks 10 %, neighbour-notify event handlers | dust power propagation computed once per wire network instead of per block update, in vanilla order | no: update order is visible |
| pistons | `pistons` | (to profile) | moving block structures and their block entities | no |
| mobs | `mobs`, `effects` | mods' goals scanning every nearby entity each tick: an `AvoidEntityGoal` over all `LivingEntity`s whose targeting check runs mod hooks (`canBeSeenByAnyone` → `hasEffect` → equipment) before the goal's own cheap filter; entity section lookups 5 % | goal selectors, pathfinding, collisions with section-level lookups | path searches are read-only and could run ahead on workers, applied in tick order |
| effects | `effects` | `hasEffect` lookups from the hooks above (the effects add ~14 ms over `mobs`) | effect ticking and attribute recomputation only on change | no |
| entities | `mobs`, `tnt` | entity section lookups, movement collisions | cached collision shapes, fewer list copies | no |
| world generation | (to add: chunk generation rate) | noise and density functions, structure placement | vanilla already generates on worker threads; widen its parallelism and cut per-chunk overhead (what C2ME does, without its mod conflicts) | yes |

"Entity Culling" is a client mod (it skips rendering hidden entities); its server-side
counterpart, ticking far-away entities less often, changes gameplay and would be opt-in.
