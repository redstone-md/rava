package dev.rava.patches.gameevents;

import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.LevelChunk;
import net.minecraft.world.level.gameevent.GameEventListenerRegistry;

/**
 * `ChunkAccess.getListenerRegistry(int)` for `GameEventDispatcher.post`. A server chunk creates
 * an empty registry for every section asked about and keeps it until a listener registered
 * there leaves; `post` only visits, and visiting an empty registry finds nothing. The hook
 * returns a section's registry when there is one, else the shared empty registry.
 */
public final class Registries {
    private Registries() {}

    public static GameEventListenerRegistry get(ChunkAccess chunk, int sectionY) {
        if (chunk instanceof LevelChunk level && level.gameEventListenerRegistrySections != null) {
            GameEventListenerRegistry r = level.gameEventListenerRegistrySections.get(sectionY);
            return r != null ? r : GameEventListenerRegistry.NOOP;
        }
        return chunk.getListenerRegistry(sectionY);
    }
}
