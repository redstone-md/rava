// Faithful repro of MC's own chunk loading: RegionFile (FileChannel-backed) over
// the real cached spawn region, hasChunk + getChunkDataInputStream for all 1024
// positions. If rava returns FOUND=0 (or different bytes) where the JVM finds
// ~144 cached chunks, that's the silent every-chunk-regenerates boot grind.
import net.minecraft.world.level.chunk.storage.RegionFile;
import net.minecraft.world.level.chunk.storage.RegionStorageInfo;
import net.minecraft.world.level.ChunkPos;
import java.nio.file.*;
import java.io.*;

public class RegionRead {
    public static void main(String[] a) throws Exception {
        RegionStorageInfo info = new RegionStorageInfo("repro", null, "chunk");
        RegionFile rf = new RegionFile(
            info, Paths.get("world/region/r.0.0.mca"), Paths.get("world/region"), true);
        int found = 0;
        long acc = 0;
        for (int x = 0; x < 32; x++) {
            for (int z = 0; z < 32; z++) {
                ChunkPos p = new ChunkPos(x, z);
                if (!rf.hasChunk(p)) continue;
                found++;
                DataInputStream in = rf.getChunkDataInputStream(p);
                if (in == null) continue;
                int b, n = 0;
                while (n < 512 && (b = in.read()) >= 0) { acc = acc * 31 + b; n++; }
                in.close();
            }
        }
        rf.close();
        System.out.println("FOUND=" + found);
        System.out.println("SUM=" + acc);
    }
}
