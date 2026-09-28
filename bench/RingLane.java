// BIT-EXACTNESS GATE FOR THE PARALLEL RING LANE.
//
// WHY THIS EXISTS. RingScan replicates the stronghold-ring loop INLINE and serially — no
// supplyAsync, no executor — so it never arms the gil-free ring lane. ChunkFill does not either.
// That left the lane, once it became a default, with no bit-exactness check at all: both existing
// gates would stay green while the parallel path computed anything it liked.
//
// This is RingScan's computation, dispatched the way Minecraft dispatches it — one
// CompletableFuture.supplyAsync per ring position, over a forked RandomSource captured per task —
// which is exactly the shape gilfree_cf admits. Run it with the lane on and with
// RAVA_NO_GILFREE_RINGS=1: the HASH must be identical, and it must also equal RingScan's HASH for
// the same N, because the per-index computation is the same.
//
// THE HASH IS ORDER-INDEPENDENT BY CONSTRUCTION. Results land in a slot indexed by i and are
// folded in index order after every task has joined, so completion order cannot affect it. That is
// the property a parallel gate needs: it must fail on a WRONG value and never on a different
// interleaving.
//
// arg0 = ring count (vanilla stronghold = 128); smaller counts scale the work.
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CompletableFuture;
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

public class RingLane {
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

        // Vanilla generateRingPositions loop shape: distance=32, spread=3. The RNG walk stays
        // SERIAL and each task gets its own fork() — exactly what ChunkGeneratorStructureState
        // does, and the reason the tasks are independent at all.
        int distance = 32, spread = 3;
        RandomSource rand = RandomSource.create();
        rand.setSeed(seed);
        double angle = rand.nextDouble() * Math.PI * 2.0;
        int inRing = 0, ring = 0, curSpread = spread;
        List<CompletableFuture<long[]>> futures = new ArrayList<>();
        for (int i = 0; i < count; i++) {
            double dist = (double) (4 * distance + distance * ring * 6)
                + (rand.nextDouble() - 0.5) * (double) distance * 2.5;
            final int cx = (int) Math.round(Math.cos(angle) * dist);
            final int cz = (int) Math.round(Math.sin(angle) * dist);
            final RandomSource forked = rand.fork();
            futures.add(CompletableFuture.supplyAsync(() -> {
                Pair<BlockPos, Holder<Biome>> pair = biomes.findBiomeHorizontal(
                    SectionPos.sectionToBlockCoord(cx, 8), 0, SectionPos.sectionToBlockCoord(cz, 8),
                    112, pred, forked, random.sampler());
                if (pair != null) {
                    BlockPos pos = pair.getFirst();
                    return new long[] { SectionPos.blockToSectionCoord(pos.getX()),
                                        SectionPos.blockToSectionCoord(pos.getZ()) };
                }
                return new long[] { cx, cz };
            }));
            angle += Math.PI * 2.0 / (double) curSpread;
            if (++inRing == curSpread) {
                ring++; inRing = 0;
                curSpread += 2 * curSpread / (ring + 1);
                curSpread = Math.min(curSpread, count - i - 1);
                angle += rand.nextDouble() * Math.PI * 2.0;
            }
        }

        long h = 0;
        for (int i = 0; i < futures.size(); i++) {
            long[] r = futures.get(i).join();
            h = h * 31 + (r[0] * 1_000_003 + r[1]);
            System.out.println("POS_" + i + " at " + r[0] + "," + r[1]);
        }
        System.out.println("LANE_MS=" + (System.nanoTime() - t1) / 1_000_000
            + " N=" + count + " HASH=" + Long.toHexString(h));
    }
}
