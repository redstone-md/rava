// The boot-shaped chunk read: SimpleRegionStorage wraps an IOWorker (dedicated
// IO thread + CompletableFuture) exactly like ChunkMap's storage. If FOUND
// drops to 0 here (while RegionRead2's direct RegionFile finds 144), the silent
// spawn-regeneration lives in the IOWorker/executor/future layer under rava.
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.util.datafix.DataFixTypes;
import net.minecraft.util.datafix.DataFixers;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.chunk.storage.RegionStorageInfo;
import net.minecraft.world.level.chunk.storage.SimpleRegionStorage;
import java.nio.file.*;
import java.util.Optional;

public class RegionRead3 {
    public static void main(String[] a) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        SimpleRegionStorage storage = new SimpleRegionStorage(
            new RegionStorageInfo("repro", null, "chunk"),
            Paths.get("world/region"),
            DataFixers.getDataFixer(),
            false,
            DataFixTypes.CHUNK);
        int found = 0;
        long acc = 0;
        for (int x = 0; x < 32; x++) {
            for (int z = 0; z < 32; z++) {
                Optional<CompoundTag> tag = storage.read(new ChunkPos(x, z)).join();
                if (tag.isPresent()) {
                    found++;
                    acc = acc * 31 + tag.get().size();
                }
            }
        }
        storage.close();
        System.out.println("FOUND=" + found);
        System.out.println("SUM=" + acc);
    }
}
