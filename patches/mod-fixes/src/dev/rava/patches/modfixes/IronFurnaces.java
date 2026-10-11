package dev.rava.patches.modfixes;

import ironfurnaces.blocks.furnaces.BlockMillionFurnace;
import net.minecraft.core.BlockPos;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.block.entity.BlockEntity;

/** Iron Furnaces: the block entity lookups of its explosion handler. */
public final class IronFurnaces {
    private IronFurnaces() {}

    /**
     * `level.getBlockEntity(pos)` where the caller only acts on a million furnace's: the block
     * entity, when the block there is a million furnace, else null (which the caller treats
     * as any other block entity). A block entity lookup reads the block state anyway, then
     * searches the chunk's block entities and asks the state whether it has one.
     */
    public static BlockEntity millionFurnaceAt(Level level, BlockPos pos) {
        return level.getBlockState(pos).getBlock() instanceof BlockMillionFurnace ? level.getBlockEntity(pos) : null;
    }
}
