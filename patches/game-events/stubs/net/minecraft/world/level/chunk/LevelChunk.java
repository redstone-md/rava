package net.minecraft.world.level.chunk;

import it.unimi.dsi.fastutil.ints.Int2ObjectMap;
import net.minecraft.world.level.gameevent.GameEventListenerRegistry;

/** Compile-time view of the members the patch opens; never packaged. */
public abstract class LevelChunk extends ChunkAccess {
    public Int2ObjectMap<GameEventListenerRegistry> gameEventListenerRegistrySections;

    private LevelChunk() {
        super(null, null, null, null, 0L, null, null);
    }
}
