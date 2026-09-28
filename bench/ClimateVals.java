// Bit-exact climate target probe: prints the 6 sampled density values at a few
// quart coords (the inputs to the RTree lookup RingScan diverges on).
import net.minecraft.SharedConstants;
import net.minecraft.core.HolderLookup;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.biome.Climate;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;

public class ClimateVals {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        RandomState random = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, 1234567L);
        Climate.Sampler s = random.sampler();
        int[][] pts = { {27, 542}, {0, 0}, {100, -100}, {-13, 570} };
        for (int[] p : pts) {
            Climate.TargetPoint t = s.sample(p[0], 0, p[1]);
            System.out.println("PT " + p[0] + "," + p[1]
                + " T=" + t.temperature() + " H=" + t.humidity()
                + " C=" + t.continentalness() + " E=" + t.erosion()
                + " D=" + t.depth() + " W=" + t.weirdness());
        }
    }
}
