# chunk-cache

A per-level cache of the last chunk looked up, for NeoForge 1.21.1 servers, with the same
results as vanilla.

Every `Level.getBlockState`, `getFluidState` and `getChunkAt` finds its chunk through
`Level.getChunk(int, int)`: three calls down to the server's chunk source, which checks the
thread, counts the lookup for the profiler and searches a four-entry cache. Block-heavy work
(explosions, redstone, collisions) asks for the same chunk many times in a row; it is the
hottest single method of the `tnt` scenario on both rava and HotSpot.

The patch adds two fields to `Level` and answers those lookups from the level's last chunk
when the lookup runs on the level's own thread and that chunk is still loaded (an unloaded
chunk is never returned: the next load makes a new object). Anything else takes the vanilla
path.

`-Drava.chunkCache.verify=true` makes the vanilla lookup as well and reports any difference.

```
patches/build.sh patches/chunk-cache <server dir>
cp patches/chunk-cache/chunk-cache.jar <server dir>/rava-patches/
```

On All The Mods 10 (rava, two rounds of two 1200-tick sprints): `redstone` 31.0 → 28.7 ms per
tick, `mobs` 78.5 → 74.4. With the verify mode, 84 million cached answers over a `redstone`
sprint were all the chunk vanilla returned.
