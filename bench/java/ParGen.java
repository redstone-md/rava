import java.util.*;
import java.util.concurrent.*;
// Chunk-generation-shaped parallel work: per task, noise sampling into arrays, small objects,
// maps and lambdas; tasks run on a work-stealing pool like a server's background executor.
public class ParGen {
    static double noise(long seed, int x, int y, int z) {
        long h = seed ^ (x * 3129871L) ^ (y * 116129781L) ^ (z * 0x5DEECE66DL);
        h = h * h * 42317861L + h * 11L;
        return ((h >> 13) & 0xFFFF) / 65535.0;
    }
    record Section(int y, short[] blocks, Map<Integer, Integer> palette) {}
    static List<Section> chunk(long seed, int cx, int cz) {
        List<Section> out = new ArrayList<>();
        for (int sy = 0; sy < 24; sy++) {
            short[] blocks = new short[4096];
            Map<Integer, Integer> palette = new HashMap<>();
            for (int i = 0; i < 4096; i++) {
                int x = (cx << 4) + (i & 15), z = (cz << 4) + ((i >> 4) & 15), y = (sy << 4) + (i >> 8);
                double d = noise(seed, x >> 2, y >> 2, z >> 2) * 2 - 1 - (y - 64) / 128.0;
                int state = d > 0 ? 1 + (int) (d * 7) : 0;
                blocks[i] = (short) (int) palette.computeIfAbsent(state, k -> palette.size());
            }
            out.add(new Section(sy, blocks, palette));
        }
        return out;
    }
    public static void main(String[] args) throws Exception {
        int threads = args.length > 0 ? Integer.parseInt(args[0]) : Runtime.getRuntime().availableProcessors();
        ForkJoinPool pool = new ForkJoinPool(threads);
        long t0 = System.nanoTime();
        List<Future<List<Section>>> fs = new ArrayList<>();
        for (int cx = -8; cx < 8; cx++) for (int cz = -8; cz < 8; cz++) { final int x = cx, z = cz; fs.add(pool.submit(() -> chunk(1234, x, z))); }
        long h = 0;
        for (Future<List<Section>> f : fs) for (Section s : f.get()) h = h * 31 + s.palette().size() + s.blocks()[100];
        System.out.println("threads=" + threads + " h=" + h + " ms=" + (System.nanoTime() - t0) / 1_000_000);
        pool.shutdown();
    }
}
