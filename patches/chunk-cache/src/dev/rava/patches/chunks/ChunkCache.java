package dev.rava.patches.chunks;

import net.minecraft.world.level.Level;
import net.minecraft.world.level.chunk.LevelChunk;

/**
 * `Level.getChunk(int, int)` with the level's last answer kept: on the level's own thread, a
 * lookup of the same chunk returns it again while it is still loaded (a chunk is a new object
 * after every unload). Other threads, and other chunks, take the vanilla path, whose answer
 * becomes the new last one.
 */
public final class ChunkCache {
    /** `-Drava.chunkCache.verify=true`: also take the vanilla path and report any difference. */
    private static final boolean VERIFY = Boolean.getBoolean("rava.chunkCache.verify");
    private static long checked, differed;

    private ChunkCache() {}

    public static LevelChunk get(Level level, int x, int z) {
        long key = (long) x << 32 | z & 0xFFFFFFFFL;
        LevelChunk last = level.ravaChunk;
        boolean owner = Thread.currentThread() == level.thread;
        if (owner && last != null && level.ravaChunkKey == key && last.loaded) {
            if (VERIFY) {
                verify(level, x, z, last);
            }
            return last;
        }
        LevelChunk chunk = level.getChunk(x, z);
        if (owner && !level.isClientSide) {
            level.ravaChunk = chunk;
            level.ravaChunkKey = key;
        }
        return chunk;
    }

    private static void verify(Level level, int x, int z, LevelChunk cached) {
        LevelChunk vanilla = level.getChunk(x, z);
        checked++;
        if (vanilla != cached && differed++ < 20) {
            System.err.println("[chunk-cache] chunk " + x + "," + z + " differs from the vanilla lookup");
        }
        if ((checked & ((1 << 24) - 1)) == 0) {
            System.err.println("[chunk-cache] verified " + checked + " lookups, " + differed + " differed");
        }
    }
}
