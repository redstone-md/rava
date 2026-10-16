# game-events

Cheaper game events for NeoForge 1.21.1 servers. Results are the same as vanilla.

Every block change, step or item drop is a game event (vibrations for sculk sensors and the
warden). `GameEventDispatcher.post` asks each chunk section within the event's radius for its
listener registry and visits it: up to 27 sections for a block change. For a section that has
none, a server chunk creates an empty registry and keeps it until a listener registered there
is removed. Every section near any activity ends up with one, and it is visited on every
event. Redstone changes blocks every tick; on All The Mods 10 `post` was a tenth of the
`redstone` scenario.

The patch makes `post` visit a section's registry only when there is one, and the shared
empty registry (`GameEventListenerRegistry.NOOP`) otherwise. Visiting an empty registry finds
nothing either way. Listeners still register through the unchanged
`getListenerRegistry` and are found exactly as before.

```
patches/build.sh patches/game-events <server dir>
cp patches/game-events/game-events.jar <server dir>/rava-patches/
```

On All The Mods 10 (rava, two rounds of two 1200-tick sprints): `redstone` 32.9 → 30.8 ms per
tick; `pistons` is unchanged within noise.
