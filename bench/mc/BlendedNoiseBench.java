import net.minecraft.util.RandomSource;
import net.minecraft.world.level.levelgen.DensityFunction;
import net.minecraft.world.level.levelgen.synth.BlendedNoise;

/** Steady-state cost of the overworld's base terrain noise, BlendedNoise.compute (three
 *  octave stacks of ImprovedNoise), with the overworld's parameters, over a column of blocks.
 *  Run with `-cp <classes>:<paper jar>:<libraries>`. */
public class BlendedNoiseBench {
    record Pos(int blockX, int blockY, int blockZ) implements DensityFunction.FunctionContext {}

    public static void main(String[] args) {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        BlendedNoise noise = new BlendedNoise(RandomSource.create(42), 0.25, 0.125, 80.0, 160.0, 8.0);
        Pos[] ps = new Pos[4096];
        for (int i = 0; i < ps.length; i++) ps[i] = new Pos((i * 37) & 255, (i % 384) - 64, (i * 101) & 255);
        double check = 0;
        for (int round = 0; round < 10; round++) {
            long t0 = System.nanoTime();
            for (int k = 0; k < 200; k++) check += batch(noise, ps);
            if (round >= 4) System.out.printf("%.1f ns/op%n", (System.nanoTime() - t0) / (200.0 * ps.length));
        }
        System.out.printf("check %.6f%n", check);
    }

    static double batch(BlendedNoise noise, Pos[] ps) {
        double s = 0;
        for (Pos p : ps) s += noise.compute(p);
        return s;
    }
}
