import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.chunk.storage.RegionFile;
import net.minecraft.world.level.chunk.storage.RegionStorageInfo;
import java.nio.file.*;
import java.io.*;

public class RegionRead8 {
    public static void main(String[] a) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        RegionStorageInfo info = new RegionStorageInfo("repro", null, "chunk");
        int[][] regions = {{-1,-1},{-1,0},{0,-1},{0,0}};
        int total = 0;
        for (int[] r : regions) {
            RegionFile rf = new RegionFile(info,
                Paths.get("world/region/r." + r[0] + "." + r[1] + ".mca"),
                Paths.get("world/region"), true);
            int found = 0;
            for (int cx = 0; cx < 32; cx++) {
                for (int cz = 0; cz < 32; cz++) {
                    ChunkPos p = new ChunkPos(r[0] * 32 + cx, r[1] * 32 + cz);
                    if (!rf.hasChunk(p)) continue;
                    DataInputStream in = rf.getChunkDataInputStream(p);
                    if (in != null) { found++; in.close(); }
                }
            }
            rf.close();
            System.out.println("r." + r[0] + "." + r[1] + " FOUND=" + found);
            total += found;
        }
        System.out.println("TOTAL=" + total);
    }
}
