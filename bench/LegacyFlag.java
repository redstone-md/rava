// Is RandomState using the legacy random path? OctaveProbe proved the
// Xoroshiro chain bit-exact, yet registry-created noises diverge with
// CORRELATED octave origins — the legacy (sequential-consumption) signature.
// If useLegacyRandomSource decodes true on rava, every noise is mis-seeded.
import net.minecraft.SharedConstants;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;

public class LegacyFlag {
    public static void main(String[] a) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        NoiseGeneratorSettings s = provider.lookupOrThrow(Registries.NOISE_SETTINGS)
            .getOrThrow(NoiseGeneratorSettings.OVERWORLD).value();
        System.out.println("LEGACY=" + s.useLegacyRandomSource());
        System.out.println("SEALEVEL=" + s.seaLevel());
        System.out.println("DISABLEMOB=" + s.disableMobGeneration());
        System.out.println("AQUIFERS=" + s.isAquifersEnabled());
        System.out.println("OREVEINS=" + s.oreVeinsEnabled());
    }
}
