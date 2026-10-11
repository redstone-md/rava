package dev.rava.patches.levels;

import java.util.function.BooleanSupplier;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.util.profiling.ProfilerFiller;
import net.minecraft.world.level.Level;

/**
 * The calls `MinecraftServer.tickChildren` makes instead of ticking each level in turn. They
 * run on the server thread.
 */
public final class Hooks {
    private static final LevelTicker TICKER = new LevelTicker();

    private Hooks() {}

    /** `EventHooks.fireLevelTickPre`: deferred until the levels run. */
    public static void pre(Level level, BooleanSupplier hasTimeLeft) {}

    /** `ServerLevel.tick`: deferred until the levels run. */
    public static void tick(ServerLevel level, BooleanSupplier hasTimeLeft) {}

    /** `EventHooks.fireLevelTickPost`: the level is queued. */
    public static void post(Level level, BooleanSupplier hasTimeLeft) {
        TICKER.queue((ServerLevel) level, hasTimeLeft);
    }

    /** `ProfilerFiller.popPush`: the first switch after the loop runs the queued levels. */
    public static void popPush(ProfilerFiller profiler, String name) {
        TICKER.run(profiler);
        profiler.popPush(name);
    }
}
