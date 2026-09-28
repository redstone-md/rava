// RIP-bracketed isolation of `RandomState.create` alone.
//
// WHY THIS EXISTS AND WHY IT IS NOT `RsProbe`. `RsProbe` prints a wall per phase, which is
// enough to A/B a lever but useless for naming one: `RandomState.create` is ~445 ms against a
// ~51 s `Bootstrap.bootStrap`, so ANY whole-run profile of that probe is a profile of the
// bootstrap. This probe warms the path, then brackets a REPEATED `RandomState.create` with the
// sampler's own phase markers (`rava.wall.reset` / `rava.wall.dump.RSCREATE`), so the dump
// block that follows covers exactly that window and nothing else.
//
// arg0 = repetitions inside the bracket (default 20 -> ~9 s of window, ~700 RIP samples at the
// sampler's real 75-87 Hz). arg1 = warmup repetitions before the bracket (default 2, which is
// what `RsProbe` shows is needed to reach the 445 ms steady state).
import net.minecraft.SharedConstants;
import net.minecraft.core.HolderLookup;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.levelgen.DensityFunction;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;

public class RsRip {
    public static void main(String[] args) throws Exception {
        int reps = args.length > 0 ? Integer.parseInt(args[0]) : 20;
        int warm = args.length > 1 ? Integer.parseInt(args[1]) : 2;

        long t0 = System.nanoTime();
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        long t1 = System.nanoTime();
        System.out.println("BOOTSTRAP_MS=" + (t1 - t0) / 1_000_000);

        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        long t2 = System.nanoTime();
        System.out.println("CREATELOOKUP_MS=" + (t2 - t1) / 1_000_000);

        long h = 1;
        for (int i = 0; i < warm; i++) {
            long s = System.nanoTime();
            RandomState r = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, 1234567L + i);
            System.out.println("WARM_MS[" + i + "]=" + (System.nanoTime() - s) / 1_000_000);
            h = h * 1000003L + probe(r, i);
        }

        // Everything tallied so far belongs to bootstrap/createLookup/warmup. The RIP buffer is
        // drained by a DUMP, not by `reset` (which only zeroes the category/method tallies), so
        // the bracket needs both: dump to discard the prologue, then reset.
        System.getProperty("rava.wall.dump.PROLOGUE");
        System.getProperty("rava.wall.reset");
        System.getProperty("rava.bridge.reset");
        long s = System.nanoTime();
        for (int i = 0; i < reps; i++) {
            RandomState r = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, 7654321L + i);
            h = h * 1000003L + probe(r, i);
        }
        long window = (System.nanoTime() - s) / 1_000_000;
        System.getProperty("rava.wall.dump.RSCREATE");
        System.getProperty("rava.bridge.dump.RSCREATE");
        System.out.println("RSCREATE_WINDOW_MS=" + window + " reps=" + reps
                + " per_call_ms=" + (window / (double) reps));
        System.out.println("RsRip_HASH=" + Long.toHexString(h));
    }

    // Same four-point evaluation `RsProbe` uses, so a hash mismatch localises to the same work.
    private static long probe(RandomState random, int i) {
        DensityFunction density = random.router().finalDensity();
        long h = 1;
        for (int x = 0; x < 4; x++) {
            double d = density.compute(new DensityFunction.SinglePointContext(x, -8, x + i));
            h = h * 1000003L + Double.doubleToRawLongBits(d);
        }
        return h;
    }
}
