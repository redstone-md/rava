import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.world.level.biome.Climate;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;

/** Steady-state cost of the overworld's climate sampler (temperature, humidity,
 *  continentalness, erosion, depth and weirdness density functions) behind every biome lookup:
 *  RandomState.sampler().sample at quart positions. Run with `-cp <classes>:<paper jar>:<libraries>`. */
public class ClimateBench {
    public static void main(String[] args) {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        HolderLookup.Provider lookup = VanillaRegistries.createLookup();
        NoiseGeneratorSettings settings = lookup.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD).value();
        RandomState random = RandomState.create(settings, lookup.lookupOrThrow(Registries.NOISE), 1234567L);
        Climate.Sampler sampler = random.sampler();
        long check = 0;
        for (int round = 0; round < 10; round++) {
            long t0 = System.nanoTime();
            for (int k = 0; k < 20; k++) check += batch(sampler, round * 20 + k);
            if (round >= 4) System.out.printf("%.1f ns/sample%n", (System.nanoTime() - t0) / (20.0 * 4096));
        }
        System.out.printf("check %d%n", check);
    }

    static long batch(Climate.Sampler sampler, int base) {
        long s = 0;
        for (int i = 0; i < 4096; i++) {
            Climate.TargetPoint t = sampler.sample(base * 16 + (i & 63), (i >> 6) - 16, (i * 7) & 1023);
            s += t.temperature() ^ t.humidity() ^ t.continentalness() ^ t.erosion() ^ t.depth() ^ t.weirdness();
        }
        return s;
    }
}
