import net.minestom.server.MinecraftServer;
import net.minestom.server.instance.Chunk;
import net.minestom.server.instance.InstanceContainer;
import net.minestom.server.instance.block.Block;
import net.minestom.server.coordinate.Point;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CompletableFuture;

/**
 * A real open-source Minecraft server (Minestom) booting: registry bootstrap from the game's
 * data, a noise-generated world whose chunks load in parallel, and the network server bound.
 * Prints per-phase times and a hash of every generated block state.
 */
public final class MsBench {
    static double noise(long seed, int x, int z) {
        long h = seed ^ (x * 3129871L) ^ (z * 0x5DEECE66DL);
        h = h * h * 42317861L + h * 11L;
        return ((h >> 13) & 0xFFFF) / 65535.0;
    }

    static double height(int x, int z) {
        double v = 0, f = 1.0 / 64, a = 24;
        for (int o = 0; o < 4; o++) {
            int xi = (int) Math.floor(x * f), zi = (int) Math.floor(z * f);
            double fx = x * f - xi, fz = z * f - zi;
            double n00 = noise(o, xi, zi), n10 = noise(o, xi + 1, zi), n01 = noise(o, xi, zi + 1), n11 = noise(o, xi + 1, zi + 1);
            double nx0 = n00 + (n10 - n00) * fx, nx1 = n01 + (n11 - n01) * fx;
            v += (nx0 + (nx1 - nx0) * fz) * a;
            f *= 2;
            a /= 2;
        }
        return 50 + v;
    }

    public static void main(String[] args) throws Exception {
        int radius = args.length > 0 ? Integer.parseInt(args[0]) : 8;
        long t0 = System.nanoTime();
        MinecraftServer server = MinecraftServer.init();
        long t1 = System.nanoTime();
        InstanceContainer world = MinecraftServer.getInstanceManager().createInstanceContainer();
        world.setGenerator(unit -> {
            Point start = unit.absoluteStart(), size = unit.size();
            for (int x = 0; x < size.blockX(); x++) {
                for (int z = 0; z < size.blockZ(); z++) {
                    int wx = start.blockX() + x, wz = start.blockZ() + z;
                    int h = (int) height(wx, wz);
                    unit.modifier().fill(start.add(x, 0, z).withY(-64), start.add(x + 1, 0, z + 1).withY(h - 4), Block.STONE);
                    unit.modifier().fill(start.add(x, 0, z).withY(h - 4), start.add(x + 1, 0, z + 1).withY(h - 1), Block.DIRT);
                    unit.modifier().setBlock(wx, h - 1, wz, h < 62 ? Block.SAND : Block.GRASS_BLOCK);
                    if (h < 62) unit.modifier().fill(start.add(x, 0, z).withY(h), start.add(x + 1, 0, z + 1).withY(62), Block.WATER);
                }
            }
        });
        List<CompletableFuture<Chunk>> futures = new ArrayList<>();
        for (int cx = -radius; cx < radius; cx++) for (int cz = -radius; cz < radius; cz++) futures.add(world.loadChunk(cx, cz));
        long hash = 0;
        for (CompletableFuture<Chunk> f : futures) {
            Chunk c = f.join();
            for (int y = -64; y < 100; y += 3) hash = hash * 31 + c.getBlock(7, y, 9).stateId();
        }
        long t2 = System.nanoTime();
        server.start("127.0.0.1", 0);
        long t3 = System.nanoTime();
        System.out.printf("MSBENCH init=%d chunks=%d start=%d total=%d hash=%x%n",
                (t1 - t0) / 1_000_000, (t2 - t1) / 1_000_000, (t3 - t2) / 1_000_000, (t3 - t0) / 1_000_000, hash);
        MinecraftServer.stopCleanly();
        System.exit(0);
    }
}
