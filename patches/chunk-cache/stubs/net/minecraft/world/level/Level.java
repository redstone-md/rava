package net.minecraft.world.level;

import net.minecraft.world.level.chunk.LevelChunk;

/** Compile-time view of the members the patch opens or adds; never packaged. */
public abstract class Level {
    public Thread thread;
    public boolean isClientSide;
    public LevelChunk ravaChunk;
    public long ravaChunkKey;

    public LevelChunk getChunk(int x, int z) {
        throw new AssertionError();
    }
}
