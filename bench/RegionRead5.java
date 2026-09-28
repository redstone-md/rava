// NEGATIVE-coordinate chunk reads: spawn chunks straddle (0,0), so 3/4 of them
// live in r.-1.-1 / r.-1.0 / r.0.-1. If negative region-coordinate math or the
// region FILE NAME construction misbehaves under rava, exactly those quadrants
// read as absent and the boot regenerates most of the spawn area.
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.util.datafix.DataFixTypes;
import net.minecraft.util.datafix.DataFixers;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.chunk.storage.RegionStorageInfo;
import net.minecraft.world.level.chunk.storage.SimpleRegionStorage;
import net.minecraft.world.level.chunk.storage.SerializableChunkData;
import java.nio.file.*;
import java.util.*;

public class RegionRead5 {
    public static void main(String[] a) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        SimpleRegionStorage storage = new SimpleRegionStorage(
            new RegionStorageInfo("repro", null, "chunk"),
            Paths.get("world/region"),
            DataFixers.getDataFixer(),
            false,
            DataFixTypes.CHUNK);
        int found = 0, full = 0;
        long acc = 0;
        for (int x = -32; x < 32; x++) {
            for (int z = -32; z < 32; z++) {
                Optional<CompoundTag> tag = storage.read(new ChunkPos(x, z)).join();
                if (tag.isEmpty()) continue;
                found++;
                acc = acc * 31 + tag.get().size();
                if ("minecraft:full".equals(String.valueOf(
                        SerializableChunkData.getChunkStatusFromTag(tag.get())))) {
                    full++;
                }
            }
        }
        storage.close();
        System.out.println("FOUND=" + found);
        System.out.println("FULL=" + full);
        System.out.println("SUM=" + acc);
    }
}
