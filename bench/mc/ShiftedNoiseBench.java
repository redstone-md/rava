package net.minecraft.world.level.levelgen;

import net.minecraft.core.Holder;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.levelgen.synth.NormalNoise;

/** Steady-state speed of the climate density functions' shape: DensityFunctions.ShiftedNoise
 *  over ShiftA/ShiftB offsets (as in the overworld's temperature, vegetation, continentalness
 *  noise), several instances behind the DensityFunction interface. In the server jar's package
 *  for its protected constructors; run with `-cp <classes>:<paper jar>:<libraries>`. */
public class ShiftedNoiseBench {
    static DensityFunction.NoiseHolder holder(RandomSource rs, int first, double... amps) {
        NormalNoise.NoiseParameters p = new NormalNoise.NoiseParameters(first, amps[0], java.util.Arrays.copyOfRange(amps, 1, amps.length));
        return new DensityFunction.NoiseHolder(Holder.direct(p), NormalNoise.create(rs, p));
    }

    public static void main(String[] args) {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        RandomSource rs = new XoroshiroRandomSource(7);
        DensityFunction.NoiseHolder offset = holder(rs, -3, 1.0, 1.0, 1.0, 0.0);
        DensityFunction sx = new DensityFunctions.ShiftA(offset), sz = new DensityFunctions.ShiftB(offset);
        DensityFunction[] fns = {
            new DensityFunctions.ShiftedNoise(sx, DensityFunctions.zero(), sz, 0.25, 0.0, holder(rs, -10, 1.5, 0.0, 1.0, 0.0, 0.0, 0.0)),
            new DensityFunctions.ShiftedNoise(sx, DensityFunctions.zero(), sz, 0.25, 0.0, holder(rs, -8, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0)),
            new DensityFunctions.ShiftedNoise(sx, DensityFunctions.zero(), sz, 0.25, 0.0, holder(rs, -9, 1.0, 1.0, 2.0, 2.0, 2.0, 1.0, 1.0, 1.0, 1.0)),
            new DensityFunctions.ShiftedNoise(sx, DensityFunctions.zero(), sz, 0.25, 0.0, holder(rs, -9, 1.0, 1.0, 0.0, 1.0, 1.0)),
        };
        double sum = 0;
        int n = 200_000;
        for (int round = 0; round < 8; round++) {
            long t0 = System.nanoTime();
            for (int i = 0; i < n; i++) {
                DensityFunction.FunctionContext c = new DensityFunction.SinglePointContext(i * 4, (i & 63) * 4 - 64, (i >> 3) * 4);
                for (DensityFunction f : fns) sum += f.compute(c);
            }
            if (round >= 3) System.out.printf("round %d %6.1f ns per point%n", round, (System.nanoTime() - t0) / (double) n);
        }
        System.out.println("check " + sum);
    }
}
