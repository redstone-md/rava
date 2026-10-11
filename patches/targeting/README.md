# targeting

Faster target searches for NeoForge 1.21.1 servers, with the same results as vanilla.

A mob goal looking for something to flee from, attack or follow searches the entities around
it with `EntityGetter.getNearestEntity` (or `getNearbyEntities`, `getNearbyPlayers`) and tests
each candidate with `TargetingConditions.test`. That test first asks whether the candidate
`canBeSeenByAnyone` and only then the goal's own selector. On a modded server the first
question runs mod hooks (on All The Mods 10: Relics → `hasEffect` → Forbidden Arcanus →
every equipment slot), while the selector is usually a cheap type test that rejects nearly
every neighbour: Ice and Fire gives every animal an `AvoidEntityGoal` over all living
entities whose selector is `instanceof IAnimalFear`. A herd of 250 cows tests every cow
against every other, through all those hooks, every tick.

The patch makes those searches ask the selector first. A candidate it rejects is rejected at
once; any other gets the whole vanilla test. Both questions are predicates without side
effects, so the answers are vanilla's. `-Drava.targeting.verify=true` runs the vanilla test
for every early rejection as well and reports any difference.

```
patches/build.sh patches/targeting <server dir>
cp patches/targeting/targeting.jar <server dir>/rava-patches/
```

On All The Mods 10 (rava, two rounds of two 1200-tick sprints): `mobs` 82.3 → 53.8 ms per
tick, `effects` 82.5 → 60.2 (`java` without the patch: 49–60 and 69). With the verify mode, 8.4
million early rejections over a `mobs` sprint all matched the vanilla test.
