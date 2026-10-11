# game-events

Cheaper game events for NeoForge 1.21.1 servers, with the same results as vanilla.

Every block change, step or item drop is a game event (vibrations for sculk sensors and the
warden). `GameEventDispatcher.post` asks each chunk section within the event's radius for its
listener registry and visits it: up to 27 sections for a block change. A server chunk answers
by creating an empty registry for a section that has none, kept until a listener registered
there is removed, so every section near any activity ends up with one, visited on every
event. Redstone changes blocks every tick; on All The Mods 10 `post` was a tenth of the
`redstone` scenario.

The patch makes `post` visit a section's registry only when there is one, and the shared
empty registry (`GameEventListenerRegistry.NOOP`) otherwise. Visiting an empty registry finds
nothing either way; listeners still register through the unchanged
`getListenerRegistry`, and are found exactly as before.

```
patches/build.sh patches/game-events <server dir>
cp patches/game-events/game-events.jar <server dir>/rava-patches/
```

On All The Mods 10 (rava, two rounds of two 1200-tick sprints): `redstone` 32.9 → 30.8 ms per
tick; `pistons` unchanged within noise.
