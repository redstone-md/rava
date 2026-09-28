// MC's OWN chunk-read path over the real cached region: Bootstrap registries,
// then RegionFile.hasChunk + getChunkDataInputStream for all 1024 positions of
// r.0.0.mca. Under the JVM this finds ~144 cached chunks; if rava finds 0 (or
// throws), the boot's silent spawn-area regeneration lives at THIS layer.
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.chunk.storage.RegionFile;
import net.minecraft.world.level.chunk.storage.RegionStorageInfo;
import java.nio.file.*;
import java.io.*;

public class RegionRead2 {
    public static void main(String[] a) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        RegionStorageInfo info = new RegionStorageInfo("repro", null, "chunk");
        RegionFile rf = new RegionFile(
            info, Paths.get("world/region/r.0.0.mca"), Paths.get("world/region"), true);
        int found = 0, readable = 0;
        long acc = 0;
        for (int x = 0; x < 32; x++) {
            for (int z = 0; z < 32; z++) {
                ChunkPos p = new ChunkPos(x, z);
                if (!rf.hasChunk(p)) continue;
                found++;
                DataInputStream in = rf.getChunkDataInputStream(p);
                if (in == null) continue;
                readable++;
                int b, n = 0;
                while (n < 256 && (b = in.read()) >= 0) { acc = acc * 31 + b; n++; }
                in.close();
            }
        }
        rf.close();
        System.out.println("FOUND=" + found);
        System.out.println("READABLE=" + readable);
        System.out.println("SUM=" + acc);
    }
}
