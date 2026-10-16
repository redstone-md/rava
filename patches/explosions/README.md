# explosions

Faster explosions for NeoForge 1.21.1 servers. Results are the same as vanilla.

An explosion damages and pushes every entity near it in proportion to how much of the entity
it "sees": `Explosion.getSeenPercent` casts up to 27 rays from points on the entity to the
explosion. A chain of TNT is thousands of explosions, each seeing hundreds of primed TNT
entities, so these rays are most of a TNT lag spike.

The patch replaces the `Level.clip` call of those rays with its own walk: the same blocks in
the same order (the arithmetic of `BlockGetter.traverseBlocks`), with the same collision
shapes, giving the same result. Blocks are read from the ray's current chunk instead of looking the chunk
up for every block, and air is skipped without asking for its empty shape. Asking would also run the
mod hooks on `getCollisionShape`, so with the patch they are not called for air.

```
patches/build.sh patches/explosions <server dir>
cp patches/explosions/explosions.jar <server dir>/rava-patches/
```

`-Drava.explosions.verify=true` also casts every ray the vanilla way and reports any
difference. Over a 9600-TNT chain (`bench/scenarios/tnt.txt`), none of about 40 million rays
differed in hit or miss, block, point or face.

On All The Mods 10 with that scenario (600 ticks, rava): 320 → 196 ms per tick (`java` without
the patch: 217). Most of what remains is mods' `ExplosionEvent.Detonate` handlers (Iron
Furnaces looks up a block entity at every affected block) and `Explosion.explode` itself.
