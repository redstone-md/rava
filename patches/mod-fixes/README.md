# mod-fixes

Fixes for costly code in popular mods, for NeoForge 1.21.1 servers. Each fix gives the mod's
results, faster; a fix whose mod is not installed, or whose code changed, does nothing (rava
reports an edit without target and leaves the class alone).

| mod | problem | fix |
|---|---|---|
| Iron Furnaces 4.3 | Its `ExplosionKnockbackEvent` handler runs once per entity per explosion, and looks up a block entity at every block the explosion destroys, looking for a million furnace. In a TNT chain that is thousands of entities times thousands of blocks. | The lookup reads the block first and asks for the block entity only where the block is a million furnace; the handler ignores every other block entity anyway. |

```
patches/build.sh patches/mod-fixes <server dir>
cp patches/mod-fixes/mod-fixes.jar <server dir>/rava-patches/
```
