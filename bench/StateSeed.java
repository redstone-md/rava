// RandomState's INPUTS: NestedFork proved the whole noise stack bit-exact
// given ("minecraft:temperature", seed 1234567). So RandomState.getOrCreateNoise
// must be feeding something else. Print the key string (len+hash+chars), the
// registry params, and a MANUAL recreation vs RandomState's own noise.
import net.minecraft.SharedConstants;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.levelgen.Noises;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;
import net.minecraft.world.level.levelgen.synth.NormalNoise;

public class StateSeed {
    public static void main(String[] a) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        String loc = Noises.TEMPERATURE.identifier().toString();
        System.out.println("LOC=[" + loc + "] LEN=" + loc.length() + " HASH=" + loc.hashCode());
        NormalNoise.NoiseParameters params = provider
            .lookupOrThrow(Registries.NOISE)
            .getOrThrow(Noises.TEMPERATURE).value();
        System.out.println("P_FIRST=" + params.firstOctave() + " P_AMPS=" + params.amplitudes());
        RandomSource rnd2 = new XoroshiroRandomSource(1234567L).forkPositional().fromHashOf(loc);
        NormalNoise manual = NormalNoise.create(rnd2, params);
        StringBuilder sb1 = new StringBuilder();
        manual.parityConfigString(sb1);
        System.out.println("MANUAL=" + sb1.substring(0, 160));
        RandomState random = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, 1234567L);
        StringBuilder sb2 = new StringBuilder();
        random.getOrCreateNoise(Noises.TEMPERATURE).parityConfigString(sb2);
        System.out.println("STATE=" + sb2.substring(0, 160));
    }
}
