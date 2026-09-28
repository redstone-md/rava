import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.util.datafix.DataFixTypes;
import net.minecraft.util.datafix.DataFixers;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.chunk.storage.RegionStorageInfo;
import net.minecraft.world.level.chunk.storage.SimpleRegionStorage;
import java.nio.file.*;
import java.util.*;

public class RegionRead7 {
    public static void main(String[] a) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        SimpleRegionStorage storage = new SimpleRegionStorage(
            new RegionStorageInfo("repro", null, "chunk"),
            Paths.get("world/region"),
            DataFixers.getDataFixer(), false, DataFixTypes.CHUNK);
        int[][] quad = new int[4][2]; // [q][found,total-with-data-under-jvm]
        for (int x = -32; x < 32; x++) {
            for (int z = -32; z < 32; z++) {
                int q = (x < 0 ? 1 : 0) | (z < 0 ? 2 : 0);
                Optional<CompoundTag> tag = storage.read(new ChunkPos(x, z)).join();
                if (tag.isPresent()) quad[q][0]++;
            }
        }
        storage.close();
        System.out.println("Q0(++)=" + quad[0][0] + " Q1(-+)=" + quad[1][0]
            + " Q2(+-)=" + quad[2][0] + " Q3(--)=" + quad[3][0]);
    }
}
