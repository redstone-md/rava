package dev.rava.patches.explosions;

import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.util.Mth;
import net.minecraft.world.level.ClipContext;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.LevelChunk;
import net.minecraft.world.level.material.FluidState;
import net.minecraft.world.phys.BlockHitResult;
import net.minecraft.world.phys.Vec3;
import net.minecraft.world.phys.shapes.VoxelShape;

/**
 * `Level.clip` for the rays of `Explosion.getSeenPercent`: the blocks of
 * `BlockGetter.traverseBlocks`, in the same order with the same arithmetic, the same test at
 * each, and the same result. It reads blocks from the ray's current chunk instead of looking
 * the chunk up for every block, and passes air (no shape, no fluid) without asking for its
 * shapes.
 */
public final class ExposureRay {
    /** `-Drava.explosions.verify=true`: also cast the vanilla ray and report any difference. */
    private static final boolean VERIFY = Boolean.getBoolean("rava.explosions.verify");
    private static long checked, differed;

    private ExposureRay() {}

    public static BlockHitResult clip(Level level, ClipContext ctx) {
        BlockHitResult fast = walk(level, ctx);
        if (VERIFY) {
            verify(level, ctx, fast);
        }
        return fast;
    }

    private static void verify(Level level, ClipContext ctx, BlockHitResult fast) {
        BlockHitResult slow = level.clip(ctx);
        boolean same = slow.getType() == fast.getType() && slow.getBlockPos().equals(fast.getBlockPos())
                && slow.getLocation().equals(fast.getLocation()) && slow.getDirection() == fast.getDirection();
        checked++;
        if (!same && differed++ < 20) {
            System.err.println("[explosions] ray differs: " + ctx.getFrom() + " -> " + ctx.getTo() + ": vanilla "
                    + slow.getType() + " " + slow.getBlockPos() + ", patch " + fast.getType() + " " + fast.getBlockPos());
        }
        if ((checked & ((1 << 20) - 1)) == 0) {
            System.err.println("[explosions] verified " + checked + " rays, " + differed + " differed");
        }
    }

    private static BlockHitResult walk(Level level, ClipContext ctx) {
        Vec3 from = ctx.getFrom(), to = ctx.getTo();
        if (from.equals(to)) {
            return level.clip(ctx);
        }
        Walk walk = new Walk(level, ctx);
        double endX = Mth.lerp(-1.0E-7, to.x, from.x), endY = Mth.lerp(-1.0E-7, to.y, from.y), endZ = Mth.lerp(-1.0E-7, to.z, from.z);
        double startX = Mth.lerp(-1.0E-7, from.x, to.x), startY = Mth.lerp(-1.0E-7, from.y, to.y), startZ = Mth.lerp(-1.0E-7, from.z, to.z);
        int x = Mth.floor(startX), y = Mth.floor(startY), z = Mth.floor(startZ);
        BlockHitResult hit = walk.test(x, y, z);
        if (hit != null) {
            return hit;
        }
        double dx = endX - startX, dy = endY - startY, dz = endZ - startZ;
        int sx = Mth.sign(dx), sy = Mth.sign(dy), sz = Mth.sign(dz);
        double stepX = sx == 0 ? Double.MAX_VALUE : sx / dx;
        double stepY = sy == 0 ? Double.MAX_VALUE : sy / dy;
        double stepZ = sz == 0 ? Double.MAX_VALUE : sz / dz;
        double tx = stepX * (sx > 0 ? 1.0 - Mth.frac(startX) : Mth.frac(startX));
        double ty = stepY * (sy > 0 ? 1.0 - Mth.frac(startY) : Mth.frac(startY));
        double tz = stepZ * (sz > 0 ? 1.0 - Mth.frac(startZ) : Mth.frac(startZ));
        while (tx <= 1.0 || ty <= 1.0 || tz <= 1.0) {
            if (tx < ty) {
                if (tx < tz) {
                    x += sx;
                    tx += stepX;
                } else {
                    z += sz;
                    tz += stepZ;
                }
            } else if (ty < tz) {
                y += sy;
                ty += stepY;
            } else {
                z += sz;
                tz += stepZ;
            }
            hit = walk.test(x, y, z);
            if (hit != null) {
                return hit;
            }
        }
        Vec3 back = from.subtract(to);
        return BlockHitResult.miss(to, Direction.getNearest(back.x, back.y, back.z), BlockPos.containing(to));
    }

    /** One ray's blocks. */
    private static final class Walk {
        private final Level level;
        private final ClipContext ctx;
        private final BlockPos.MutableBlockPos pos = new BlockPos.MutableBlockPos();
        private LevelChunk chunk;
        private int chunkX = Integer.MIN_VALUE, chunkZ = Integer.MIN_VALUE;

        Walk(Level level, ClipContext ctx) {
            this.level = level;
            this.ctx = ctx;
        }

        BlockHitResult test(int x, int y, int z) {
            if (level.isOutsideBuildHeight(y)) {
                return null;
            }
            int cx = x >> 4, cz = z >> 4;
            if (cx != chunkX || cz != chunkZ) {
                chunk = level.getChunk(cx, cz);
                chunkX = cx;
                chunkZ = cz;
            }
            pos.set(x, y, z);
            BlockState state = chunk.getBlockState(pos);
            if (state.isAir()) {
                return null;
            }
            // What BlockGetter.clip tests at a block, unchanged.
            Vec3 from = ctx.getFrom(), to = ctx.getTo();
            VoxelShape shape = ctx.getBlockShape(state, level, pos);
            BlockHitResult block = level.clipWithInteractionOverride(from, to, pos, shape, state);
            FluidState fluidState = state.getFluidState();
            BlockHitResult fluid = fluidState.isEmpty() ? null : ctx.getFluidShape(fluidState, level, pos).clip(from, to, pos);
            double toBlock = block == null ? Double.MAX_VALUE : from.distanceToSqr(block.getLocation());
            double toFluid = fluid == null ? Double.MAX_VALUE : from.distanceToSqr(fluid.getLocation());
            return toBlock <= toFluid ? block : fluid;
        }
    }
}
