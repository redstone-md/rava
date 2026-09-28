// M3 multicore polygon — a COORDINATE-INDEPENDENT chunk noise-fill, the ideal RavaPool target.
// Mirrors RingScan.java's bootstrap (VanillaRegistries + the OVERWORLD noise router via RandomState),
// then does what fillFromNoise/NoiseChunk does at its core: evaluate the terrain density function at
// every (x,y,z) block of an NxN column stack. UNLIKE RingScan's ring loop, there is NO loop-carried
// RNG / no cross-iteration state: finalDensity().compute(x,y,z) is a PURE function of the coordinate +
// the (read-only, pre-wired) router. Every iteration is independent → embarrassingly parallel. The
// densities fold into a strict-ordered HASH (order-independent math would also work, but ordered keeps
// serial==parallel provable). arg0 = chunks-per-side (grid = arg0*16 wide), arg1 = column height.
import net.minecraft.SharedConstants;
import net.minecraft.core.HolderLookup;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.levelgen.DensityFunction;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;

public class ChunkFill {
    public static void main(String[] args) throws Exception {
        int side = args.length > 0 ? Integer.parseInt(args[0]) : 1;   // chunks per side
        int height = args.length > 1 ? Integer.parseInt(args[1]) : 64; // y-cells per column
        long seed = 1234567L;

        long t0 = System.nanoTime();
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        RandomState random = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, seed);
        DensityFunction density = random.router().finalDensity();
        long t1 = System.nanoTime();
        System.out.println("SETUP_MS=" + (t1 - t0) / 1_000_000);

        int span = side * 16;   // block width/depth
        int yMin = -32;
        int yEnd = yMin + height;   // single-local loop bound (standard javac idiom)
        long t2 = System.nanoTime();
        long h = 1;
        // COORDINATE MAP: each (x,y,z) density is independent — the target the fork will partition.
        for (int x = 0; x < span; x++) {
            for (int z = 0; z < span; z++) {
                for (int y = yMin; y < yEnd; y++) {
                    double d = density.compute(new DensityFunction.SinglePointContext(x, y, z));
                    h = h * 1000003L + Double.doubleToRawLongBits(d);
                }
            }
        }
        long t3 = System.nanoTime();
        long cells = (long) span * span * height;
        System.out.println("FILL_MS=" + (t3 - t2) / 1_000_000 + " CELLS=" + cells
            + " SIDE=" + side + " HEIGHT=" + height + " ChunkFill_HASH=" + Long.toHexString(h));
    }
}
