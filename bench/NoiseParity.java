// Dump MC's own noise parity config for one registry noise: firstOctave,
// amplitudes, per-octave ImprovedNoise origins + perm samples. Diff JVM vs
// rava pins which component of the seeded noise diverges.
import net.minecraft.SharedConstants;
import net.minecraft.core.HolderLookup;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.levelgen.Noises;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;

public class NoiseParity {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        RandomState random = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, 1234567L);
        StringBuilder sb = new StringBuilder();
        random.getOrCreateNoise(Noises.TEMPERATURE).parityConfigString(sb);
        System.out.println("TEMP_PARITY=" + sb);
    }
}
