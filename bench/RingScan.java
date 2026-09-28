// Standalone repro of the stronghold-ring boot phase
// (ChunkGeneratorStructureState.generateRingPositions): the real overworld
// noise router + Climate/RTree biome scan, minus the rest of boot. The ring
// loop is replicated inline (vanilla's private method needs a bound-tags
// registry the datagen lookup can't provide) — the cost is entirely in
// findBiomeHorizontal, which is the real thing.
// arg0 = ring count (vanilla stronghold = 128); smaller counts scale the work.
// Deterministic: HASH comparable between JVM and rava at any count.
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

public class RingScan {
    public static void main(String[] args) throws Exception {
        int count = args.length > 0 ? Integer.parseInt(args[0]) : 8;
        long seed = 1234567L;
        long t0 = System.nanoTime();
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        RandomState random = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, seed);
        MultiNoiseBiomeSource biomes = MultiNoiseBiomeSource.createFromPreset(
            provider.lookupOrThrow(Registries.MULTI_NOISE_BIOME_SOURCE_PARAMETER_LIST)
                .getOrThrow(MultiNoiseBiomeSourceParameterLists.OVERWORLD));
        HolderLookup.RegistryLookup<Biome> biomeReg = provider.lookupOrThrow(Registries.BIOME);
        HolderSet<Biome> preferred = HolderSet.direct(
            biomeReg.getOrThrow(Biomes.PLAINS), biomeReg.getOrThrow(Biomes.DESERT),
            biomeReg.getOrThrow(Biomes.TAIGA), biomeReg.getOrThrow(Biomes.FOREST),
            biomeReg.getOrThrow(Biomes.SNOWY_PLAINS));
        Predicate<Holder<Biome>> pred = preferred::contains;
        long t1 = System.nanoTime();
        System.out.println("SETUP_MS=" + (t1 - t0) / 1_000_000);
        // Vanilla generateRingPositions loop shape: distance=32, spread=3.
        int distance = 32, spread = 3;
        RandomSource rand = RandomSource.create();
        rand.setSeed(seed);
        double angle = rand.nextDouble() * Math.PI * 2.0;
        int inRing = 0, ring = 0, curSpread = spread;
        long h = 0;
        for (int i = 0; i < count; i++) {
            double dist = (double) (4 * distance + distance * ring * 6)
                + (rand.nextDouble() - 0.5) * (double) distance * 2.5;
            int cx = (int) Math.round(Math.cos(angle) * dist);
            int cz = (int) Math.round(Math.sin(angle) * dist);
            RandomSource forked = rand.fork();
            long ts = System.nanoTime();
            Pair<BlockPos, Holder<Biome>> pair = biomes.findBiomeHorizontal(
                SectionPos.sectionToBlockCoord(cx, 8), 0, SectionPos.sectionToBlockCoord(cz, 8),
                112, pred, forked, random.sampler());
            long px, pz;
            if (pair != null) {
                BlockPos pos = pair.getFirst();
                px = SectionPos.blockToSectionCoord(pos.getX());
                pz = SectionPos.blockToSectionCoord(pos.getZ());
            } else {
                px = cx; pz = cz;
            }
            h = h * 31 + (px * 1_000_003 + pz);
            System.out.println("POS_" + i + "_MS=" + (System.nanoTime() - ts) / 1_000_000
                + " at " + px + "," + pz);
            angle += Math.PI * 2.0 / (double) curSpread;
            if (++inRing == curSpread) {
                ring++; inRing = 0;
                curSpread += 2 * curSpread / (ring + 1);
                curSpread = Math.min(curSpread, count - i - 1);
                angle += rand.nextDouble() * Math.PI * 2.0;
            }
        }
        System.out.println("RINGS_MS=" + (System.nanoTime() - t1) / 1_000_000
            + " N=" + count + " HASH=" + Long.toHexString(h));
    }
}
