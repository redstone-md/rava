# chunk-cache

A per-level cache of the last chunk looked up, for NeoForge 1.21.1 servers. Results are the
same as vanilla.

Every `Level.getBlockState`, `getFluidState` and `getChunkAt` finds its chunk through
`Level.getChunk(int, int)`: three calls down to the server's chunk source, which checks the
thread, counts the lookup for the profiler and searches a four-entry cache. Block-heavy work
(explosions, redstone, collisions) asks for the same chunk many times in a row. In the `tnt`
scenario this is the hottest single method, on rava and on HotSpot.

The patch adds two fields to `Level` and answers those lookups from the level's last chunk
when the lookup runs on the level's own thread and that chunk is still loaded (an unloaded
chunk is never returned, because the next load makes a new object). Every other lookup takes
the vanilla path.

`-Drava.chunkCache.verify=true` also does the vanilla lookup and reports any difference.

```
patches/build.sh patches/chunk-cache <server dir>
cp patches/chunk-cache/chunk-cache.jar <server dir>/rava-patches/
```

On All The Mods 10 (rava, two rounds of two 1200-tick sprints): `redstone` 31.0 → 28.7 ms per
tick, `mobs` 78.5 → 74.4. In verify mode, all 84 million cached answers over a `redstone`
sprint matched the chunk vanilla returned.
