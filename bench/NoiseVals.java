// Bit-exact noise probe, one level below ClimateVals: raw NormalNoise values
// (temperature/vegetation/continentalness/erosion + offset shift) from the
// seeded RandomState, plus a registry-free Xoroshiro-seeded NormalNoise —
// separates table-seeding divergence from density-function-eval divergence.
import net.minecraft.SharedConstants;
import net.minecraft.core.HolderLookup;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.levelgen.Noises;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;
import net.minecraft.world.level.levelgen.synth.NormalNoise;

public class NoiseVals {
    public static void main(String[] args) throws Exception {
        // Registry-free: pure Xoroshiro + NormalNoise (no bootstrap needed yet).
        NormalNoise plain = NormalNoise.create(new XoroshiroRandomSource(1234567L), -7, 1.0, 1.0);
        System.out.println("PLAIN=" + bits(plain.getValue(108.0, 0.0, 2168.0))
            + "," + bits(plain.getValue(0.5, -3.25, 7.75)));

        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        RandomState random = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, 1234567L);
        double[][] pts = { {108.0, 0.0, 2168.0}, {0.0, 0.0, 0.0}, {400.0, 0.0, -400.0} };
        for (double[] p : pts) {
            System.out.println("N " + (long) p[0] + "," + (long) p[2]
                + " T=" + bits(random.getOrCreateNoise(Noises.TEMPERATURE).getValue(p[0], p[1], p[2]))
                + " V=" + bits(random.getOrCreateNoise(Noises.VEGETATION).getValue(p[0], p[1], p[2]))
                + " C=" + bits(random.getOrCreateNoise(Noises.CONTINENTALNESS).getValue(p[0], p[1], p[2]))
                + " E=" + bits(random.getOrCreateNoise(Noises.EROSION).getValue(p[0], p[1], p[2]))
                + " S=" + bits(random.getOrCreateNoise(Noises.SHIFT).getValue(p[0], p[1], p[2])));
        }
    }

    static String bits(double d) { return Long.toHexString(Double.doubleToLongBits(d)); }
}
