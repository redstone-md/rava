// M3 column/position-level fork prototype. RingScan restructured into 3 phases so the expensive
// INDEPENDENT per-position findBiomeHorizontal (phase B) can fork across workers, while the
// order-dependent RNG/angle prefix (phase A) and the hash reduction (phase C) stay serial. The RNG
// sequence (rand.nextDouble/fork) is drained IN ORDER in phase A, so `forked[i]` is identical for
// any worker count → bit-exact HASH vs RingScan. No per-position println/nanoTime (IO in the loop
// would be a side effect that blocks the fork).
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

public class RingFork {
    public static void main(String[] args) throws Exception {
        int count = args.length > 0 ? Integer.parseInt(args[0]) : 8;
        long seed = 1234567L;
        long tSetup0 = System.nanoTime();
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();   // block registration + shape-cache build (the forkable geometry)
        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        System.out.println("SETUP_MS=" + (System.nanoTime() - tSetup0) / 1_000_000);
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

        int distance = 32, spread = 3;
        long best = Long.MAX_VALUE, sum = 0;
        for (int rep = 0; rep < 3; rep++) {
            int[] cx = new int[count];
            int[] cz = new int[count];
            RandomSource[] forked = new RandomSource[count];
            long[] px = new long[count];
            long[] pz = new long[count];

            // ── Phase A: SERIAL RNG/angle prefix — same draw order as RingScan.
            RandomSource rand = RandomSource.create();
            rand.setSeed(seed);
            double angle = rand.nextDouble() * Math.PI * 2.0;
            int inRing = 0, ring = 0, curSpread = spread;
            for (int i = 0; i < count; i++) {
                double dist = (double) (4 * distance + distance * ring * 6)
                    + (rand.nextDouble() - 0.5) * (double) distance * 2.5;
                cx[i] = (int) Math.round(Math.cos(angle) * dist);
                cz[i] = (int) Math.round(Math.sin(angle) * dist);
                forked[i] = rand.fork();
                angle += Math.PI * 2.0 / (double) curSpread;
                if (++inRing == curSpread) {
                    ring++; inRing = 0;
                    curSpread += 2 * curSpread / (ring + 1);
                    curSpread = Math.min(curSpread, count - i - 1);
                    angle += rand.nextDouble() * Math.PI * 2.0;
                }
            }

            // ── Phase B: INDEPENDENT per-position biome scan — the fork target.
            long t0 = System.nanoTime();
            scan(biomes, random, pred, cx, cz, forked, px, pz, count);
            long dt = System.nanoTime() - t0;
            if (dt < best) best = dt;

            // ── Phase C: SERIAL hash reduction (order-dependent).
            long h = 0;
            for (int i = 0; i < count; i++) {
                h = h * 31 + (px[i] * 1_000_003 + pz[i]);
            }
            sum = h;
        }
        System.out.println("HOTNS=" + best + " N=" + count + " HASH=" + Long.toHexString(sum));
    }

    // The forkable phase-B loop, isolated in its own method so the counted-loop fork sees a clean
    // `for (i<n) { px[i]=…; pz[i]=… }` whose body's expensive call is offloadable + independent.
    static void scan(MultiNoiseBiomeSource biomes, RandomState random, Predicate<Holder<Biome>> pred,
                     int[] cx, int[] cz, RandomSource[] forked, long[] px, long[] pz, int n) {
        for (int i = 0; i < n; i++) {
            Pair<BlockPos, Holder<Biome>> pair = biomes.findBiomeHorizontal(
                SectionPos.sectionToBlockCoord(cx[i], 8), 0, SectionPos.sectionToBlockCoord(cz[i], 8),
                112, pred, forked[i], random.sampler());
            if (pair != null) {
                BlockPos pos = pair.getFirst();
                px[i] = SectionPos.blockToSectionCoord(pos.getX());
                pz[i] = SectionPos.blockToSectionCoord(pos.getZ());
            } else {
                px[i] = cx[i];
                pz[i] = cz[i];
            }
        }
    }
}
