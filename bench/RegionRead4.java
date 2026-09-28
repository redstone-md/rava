// The regenerate-or-load DECIDER: read each cached chunk tag (storage layer
// already proven bit-exact) and ask SerializableChunkData.getChunkStatusFromTag
// — if rava returns EMPTY where the JVM returns minecraft:full, the chunk
// system regenerates everything even though the bytes load fine.
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

public class RegionRead4 {
    public static void main(String[] a) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        SimpleRegionStorage storage = new SimpleRegionStorage(
            new RegionStorageInfo("repro", null, "chunk"),
            Paths.get("world/region"),
            DataFixers.getDataFixer(),
            false,
            DataFixTypes.CHUNK);
        Map<String, Integer> statuses = new TreeMap<>();
        int found = 0;
        for (int x = 0; x < 32; x++) {
            for (int z = 0; z < 32; z++) {
                Optional<CompoundTag> tag = storage.read(new ChunkPos(x, z)).join();
                if (tag.isEmpty()) continue;
                found++;
                String st = String.valueOf(SerializableChunkData.getChunkStatusFromTag(tag.get()));
                statuses.merge(st, 1, Integer::sum);
            }
        }
        storage.close();
        System.out.println("FOUND=" + found);
        System.out.println("STATUSES=" + statuses);
    }
}
