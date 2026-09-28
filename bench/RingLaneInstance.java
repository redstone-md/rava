// BIT-EXACTNESS GATE FOR THE ADMISSION SHAPE THE BOOT ACTUALLY USES.
//
// RingLane's supplier is a lambda inside `main`, i.e. a STATIC method reference — MethodHandle
// kind 6, receiver-free. The boot's ring phase admits
// `ChunkGeneratorStructureState::lambda$generateRingPositions$5`, which is an INSTANCE lambda:
// kind 5/7, with the enclosing object captured into local slot 0 ahead of the declared captures.
// That is a different marshalling shape — `jit_direct_resolve`'s `shift` entry drops raw[0], and
// the freeze table's serve arm hands the buffer over untouched — so a gate that only exercises the
// static shape leaves the boot's own shape unproven. An audit caught exactly that gap.
//
// So this is RingLane's computation moved into an INSTANCE method of an instance object, with the
// supplier capturing `this` implicitly. Same hash contract: it must equal RingScan(N)'s hash, with
// the lane armed and disarmed, and it is folded in INDEX order so completion order cannot enter it.
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

public class RingLaneInstance {
    // INSTANCE state, so the supplier below captures `this` and the lambda is kind 5/7 rather
    // than kind 6 — the whole point of this gate.
    private MultiNoiseBiomeSource biomes;
    private RandomState random;
    private Predicate<Holder<Biome>> pred;

    public static void main(String[] args) throws Exception {
        new RingLaneInstance().run(args.length > 0 ? Integer.parseInt(args[0]) : 8);
    }

    private void run(int count) throws Exception {
        long seed = 1234567L;
        long t0 = System.nanoTime();
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider provider = VanillaRegistries.createLookup();
        this.random = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, seed);
        this.biomes = MultiNoiseBiomeSource.createFromPreset(
            provider.lookupOrThrow(Registries.MULTI_NOISE_BIOME_SOURCE_PARAMETER_LIST)
                .getOrThrow(MultiNoiseBiomeSourceParameterLists.OVERWORLD));
        HolderLookup.RegistryLookup<Biome> biomeReg = provider.lookupOrThrow(Registries.BIOME);
        HolderSet<Biome> preferred = HolderSet.direct(
            biomeReg.getOrThrow(Biomes.PLAINS), biomeReg.getOrThrow(Biomes.DESERT),
            biomeReg.getOrThrow(Biomes.TAIGA), biomeReg.getOrThrow(Biomes.FOREST),
            biomeReg.getOrThrow(Biomes.SNOWY_PLAINS));
        this.pred = preferred::contains;
        long t1 = System.nanoTime();
        System.out.println("SETUP_MS=" + (t1 - t0) / 1_000_000);

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
            // `this.at(...)` — the supplier captures the enclosing instance, so the synthetic
            // lambda body is an INSTANCE method and the call site takes the receiver-shifted path.
            futures.add(CompletableFuture.supplyAsync(() -> this.at(cx, cz, forked)));
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

    /// The per-position body, as an INSTANCE method: identical arithmetic to RingLane's, reached
    /// through the receiver-carrying lambda shape.
    private long[] at(int cx, int cz, RandomSource forked) {
        Pair<BlockPos, Holder<Biome>> pair = this.biomes.findBiomeHorizontal(
            SectionPos.sectionToBlockCoord(cx, 8), 0, SectionPos.sectionToBlockCoord(cz, 8),
            112, this.pred, forked, this.random.sampler());
        if (pair != null) {
            BlockPos pos = pair.getFirst();
            return new long[] { SectionPos.blockToSectionCoord(pos.getX()),
                                SectionPos.blockToSectionCoord(pos.getZ()) };
        }
        return new long[] { cx, cz };
    }
}
