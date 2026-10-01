import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.LevelChunkSection;
import net.minecraft.world.level.chunk.PalettedContainer;

/** Steady-state cost of a block write into a chunk section, the path behind every
 *  ProtoChunk.setBlockState in world generation: LevelChunkSection.setBlockState ->
 *  PalettedContainer.getAndSet (acquire/release, the synchronized getAndSet(int, T), palette
 *  idFor, bit storage getAndSet). Fresh sections are filled with a few states, as terrain fill
 *  does. Run with `-cp <classes>:<paper jar>:<libraries>` [rounds]. */
public class PaletteBench {
    public static void main(String[] args) {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        BlockState air = Blocks.AIR.defaultBlockState();
        BlockState[] states = {Blocks.STONE.defaultBlockState(), Blocks.WATER.defaultBlockState(), Blocks.DIRT.defaultBlockState(), Blocks.DEEPSLATE.defaultBlockState(), air};
        long check = 0;
        int rounds = args.length > 0 ? Integer.parseInt(args[0]) : 10;
        for (int round = 0; round < rounds; round++) {
            long t0 = System.nanoTime();
            int writes = 0;
            for (int s = 0; s < 400; s++) {
                LevelChunkSection sec = new LevelChunkSection(new PalettedContainer<>(Block.BLOCK_STATE_REGISTRY, air, PalettedContainer.Strategy.SECTION_STATES), null);
                check += fill(sec, states, air, s);
                writes += 4096;
            }
            if (round >= rounds - 6) System.out.printf("%.1f ns per write%n", (System.nanoTime() - t0) / (double) writes);
        }
        System.out.printf("check %d%n", check);
    }

    static long fill(LevelChunkSection sec, BlockState[] states, BlockState air, int s) {
        long n = 0;
        for (int y = 0; y < 16; y++) {
            for (int z = 0; z < 16; z++) {
                for (int x = 0; x < 16; x++) {
                    BlockState st = states[(y * 7 + ((x ^ z) & 3) + s) % states.length];
                    n += sec.setBlockState(x, y, z, st, false) == air ? 1 : 0;
                }
            }
        }
        return n;
    }
}
