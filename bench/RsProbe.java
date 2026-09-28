// Isolates the two named owners of the boot's SERIAL residue so each can be timed
// without a 200s full boot: `VanillaRegistries.createLookup()` (block C's biome
// parameter-list / RTree build) and `RandomState.create()` (the pre-ring residue's
// largest single item, run once per ServerLevel in the real boot).
// arg0 = RandomState.create repetitions (the real boot does 3, one per level).
import net.minecraft.SharedConstants;
import net.minecraft.core.HolderLookup;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.levelgen.DensityFunction;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;

public class RsProbe {
    public static void main(String[] args) throws Exception {
        int reps = args.length > 0 ? Integer.parseInt(args[0]) : 3;

        long t0 = System.nanoTime();
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        long t1 = System.nanoTime();
        System.out.println("BOOTSTRAP_MS=" + (t1 - t0) / 1_000_000);

        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        long t2 = System.nanoTime();
        System.out.println("CREATELOOKUP_MS=" + (t2 - t1) / 1_000_000);

        long h = 1;
        for (int i = 0; i < reps; i++) {
            long s = System.nanoTime();
            RandomState random = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, 1234567L + i);
            long e = System.nanoTime();
            DensityFunction density = random.router().finalDensity();
            for (int x = 0; x < 4; x++) {
                double d = density.compute(new DensityFunction.SinglePointContext(x, -8, x + i));
                h = h * 1000003L + Double.doubleToRawLongBits(d);
            }
            System.out.println("RS_CREATE_MS[" + i + "]=" + (e - s) / 1_000_000);
        }
        System.out.println("RsProbe_HASH=" + Long.toHexString(h));
    }
}
