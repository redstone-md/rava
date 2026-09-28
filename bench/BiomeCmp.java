// Divergence isolator for RingScan: replays position 0's findBiomeHorizontal
// with a RECORDING predicate. The predicate sees every sampled biome in scan
// order — if the visited-sequence hash matches JVM but the result differs, the
// culprit is RandomSource; if the sequence differs, it's Climate sampling.
// Also prints a raw RandomSource parity block (bit-exact doubles + fork).
import java.util.ArrayList;
import java.util.List;
import java.util.function.Predicate;
import com.mojang.datafixers.util.Pair;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.HolderSet;
import net.minecraft.core.SectionPos;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.Biomes;
import net.minecraft.world.level.biome.MultiNoiseBiomeSource;
import net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterLists;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;

public class BiomeCmp {
    public static void main(String[] args) throws Exception {
        // --- RNG parity (no worldgen involved) ---
        RandomSource r = RandomSource.create();
        r.setSeed(1234567L);
        StringBuilder rng = new StringBuilder("RNG=");
        for (int i = 0; i < 4; i++) rng.append(Long.toHexString(Double.doubleToLongBits(r.nextDouble()))).append(',');
        RandomSource f = r.fork();
        for (int i = 0; i < 5; i++) rng.append(f.nextInt(1000)).append(',');
        System.out.println(rng);

        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        long seed = 1234567L;
        RandomState random = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, seed);
        MultiNoiseBiomeSource biomes = MultiNoiseBiomeSource.createFromPreset(
            provider.lookupOrThrow(Registries.MULTI_NOISE_BIOME_SOURCE_PARAMETER_LIST)
                .getOrThrow(MultiNoiseBiomeSourceParameterLists.OVERWORLD));
        HolderLookup.RegistryLookup<Biome> biomeReg = provider.lookupOrThrow(Registries.BIOME);
        HolderSet<Biome> preferred = HolderSet.direct(
            biomeReg.getOrThrow(Biomes.PLAINS), biomeReg.getOrThrow(Biomes.DESERT),
            biomeReg.getOrThrow(Biomes.TAIGA), biomeReg.getOrThrow(Biomes.FOREST),
            biomeReg.getOrThrow(Biomes.SNOWY_PLAINS));

        // Same center math as RingScan position 0.
        RandomSource rand = RandomSource.create();
        rand.setSeed(seed);
        double angle = rand.nextDouble() * Math.PI * 2.0;
        double dist = (double) (4 * 32) + (rand.nextDouble() - 0.5) * 32.0 * 2.5;
        int cx = (int) Math.round(Math.cos(angle) * dist);
        int cz = (int) Math.round(Math.sin(angle) * dist);
        RandomSource forked = rand.fork();
        System.out.println("CENTER=" + cx + "," + cz);

        List<String> visited = new ArrayList<>();
        Predicate<Holder<Biome>> pred = h -> {
            visited.add(h.unwrapKey().map(k -> k.identifier().toString()).orElse("?"));
            return preferred.contains(h);
        };
        Pair<BlockPos, Holder<Biome>> pair = biomes.findBiomeHorizontal(
            SectionPos.sectionToBlockCoord(cx, 8), 0, SectionPos.sectionToBlockCoord(cz, 8),
            112, pred, forked, random.sampler());
        long h = 0;
        int matches = 0;
        for (String s : visited) { h = h * 31 + s.hashCode(); }
        for (String s : visited) if (!s.isEmpty()) {}
        for (int i = 0; i < visited.size(); i++) if (preferredName(visited.get(i))) matches++;
        System.out.println("VISITED=" + visited.size() + " SEQHASH=" + Long.toHexString(h)
            + " MATCHES=" + matches);
        // First/last few for eyeballing a mismatch.
        for (int i = 0; i < Math.min(8, visited.size()); i++)
            System.out.println("V_" + i + "=" + visited.get(i));
        System.out.println("RESULT=" + (pair == null ? "null"
            : pair.getFirst() + " " + pair.getSecond().unwrapKey().map(k -> k.identifier().toString()).orElse("?")));
    }

    static boolean preferredName(String s) {
        return s.equals("minecraft:plains") || s.equals("minecraft:desert") || s.equals("minecraft:taiga")
            || s.equals("minecraft:forest") || s.equals("minecraft:snowy_plains");
    }
}
