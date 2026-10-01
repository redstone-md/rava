import com.mojang.serialization.Lifecycle;
import java.util.List;
import java.util.function.Predicate;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.MappedRegistry;
import net.minecraft.core.RegistrationInfo;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.StructureManager;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.BiomeManager;
import net.minecraft.world.level.biome.MultiNoiseBiomeSource;
import net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterList;
import net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterLists;
import net.minecraft.world.level.chunk.ProtoChunk;
import net.minecraft.world.level.chunk.UpgradeData;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.WorldGenerationContext;
import net.minecraft.world.level.levelgen.blending.Blender;
import net.minecraft.world.level.levelgen.structure.Structure;
import net.minecraft.world.level.levelgen.structure.StructureStart;

/** Steady-state world generation on the real generator: the vanilla overworld (registries from
 *  VanillaRegistries.createLookup, the overworld noise settings and multi-noise preset), chunks
 *  taken through biomes, noise fill and surface, with no structures. Prints milliseconds per
 *  chunk for each step; each round generates chunks at new positions.
 *  Run with `-cp <classes>:<paper jar>:<libraries>` [rounds] [chunks per round]. */
public class WorldGenBench {
    static final class NoStructures extends StructureManager {
        NoStructures() { super(null, null, null); }
        @Override public List<StructureStart> startsForStructure(ChunkPos pos, Predicate<Structure> filter) { return List.of(); }
    }

    public static void main(String[] args) {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        int rounds = args.length > 0 ? Integer.parseInt(args[0]) : 12;
        int perRound = args.length > 1 ? Integer.parseInt(args[1]) : 64;
        HolderLookup.Provider lookup = VanillaRegistries.createLookup();
        Holder<NoiseGeneratorSettings> settings = lookup.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD);
        Holder<MultiNoiseBiomeSourceParameterList> preset = lookup.lookupOrThrow(Registries.MULTI_NOISE_BIOME_SOURCE_PARAMETER_LIST).getOrThrow(MultiNoiseBiomeSourceParameterLists.OVERWORLD);
        NoiseBasedChunkGenerator gen = new NoiseBasedChunkGenerator(MultiNoiseBiomeSource.createFromPreset(preset), settings);
        long seed = 1234567L;
        RandomState random = RandomState.create(settings.value(), lookup.lookupOrThrow(Registries.NOISE), seed);
        MappedRegistry<Biome> biomes = new MappedRegistry<>(Registries.BIOME, Lifecycle.stable());
        lookup.lookupOrThrow(Registries.BIOME).listElements().forEach(h -> biomes.register(h.key(), h.value(), RegistrationInfo.BUILT_IN));
        biomes.freeze();
        LevelHeightAccessor height = LevelHeightAccessor.create(-64, 384);
        StructureManager structures = new NoStructures();
        WorldGenerationContext context = new WorldGenerationContext(gen, height);
        long check = 0;
        for (int round = 0; round < rounds; round++) {
            long tb = 0, tn = 0, ts = 0;
            for (int i = 0; i < perRound; i++) {
                ChunkPos pos = new ChunkPos(round * 17 + (i & 7), i >> 3);
                ProtoChunk c = new ProtoChunk(pos, UpgradeData.EMPTY, height, biomes, null);
                long t0 = System.nanoTime();
                gen.createBiomes(random, Blender.empty(), structures, c).join();
                c.setPersistedStatus(ChunkStatus.BIOMES);
                long t1 = System.nanoTime();
                gen.fillFromNoise(Blender.empty(), random, structures, c).join();
                c.setPersistedStatus(ChunkStatus.NOISE);
                long t2 = System.nanoTime();
                gen.buildSurface(c, context, random, structures, new BiomeManager(c, BiomeManager.obfuscateSeed(seed)), biomes, Blender.empty());
                long t3 = System.nanoTime();
                tb += t1 - t0; tn += t2 - t1; ts += t3 - t2;
                // The generated blocks, by registry id, down two columns (identical on every VM).
                for (int y = -64; y < 320; y += 3) {
                    check = check * 31 + net.minecraft.world.level.block.Block.getId(c.getBlockState(new net.minecraft.core.BlockPos(pos.getMinBlockX() + 3, y, pos.getMinBlockZ() + 5)));
                    check = check * 31 + net.minecraft.world.level.block.Block.getId(c.getBlockState(new net.minecraft.core.BlockPos(pos.getMinBlockX() + 12, y, pos.getMinBlockZ() + 9)));
                }
            }
            System.out.printf("round %d: biomes %.2f noise %.2f surface %.2f ms/chunk%n", round, tb / 1e6 / perRound, tn / 1e6 / perRound, ts / 1e6 / perRound);
        }
        System.out.printf("check %d%n", check);
    }
}
