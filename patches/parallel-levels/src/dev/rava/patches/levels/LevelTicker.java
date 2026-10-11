package dev.rava.patches.levels;

import dev.rava.patches.levels.bus.EventLock;
import java.lang.reflect.Field;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.IdentityHashMap;
import java.util.List;
import java.util.Map;
import java.util.function.BooleanSupplier;
import net.minecraft.CrashReport;
import net.minecraft.ReportedException;
import net.minecraft.server.level.ServerChunkCache;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.util.profiling.InactiveProfiler;
import net.minecraft.util.profiling.ProfilerFiller;
import net.minecraft.util.thread.BlockableEventLoop;
import net.minecraft.world.level.Level;
import net.neoforged.neoforge.event.EventHooks;

/**
 * Ticks the levels queued during one server tick. The server thread fires every level's
 * `LevelTickEvent.Pre`, then ticks the levels with a few workers, which take them longest
 * first (by their last tick's time), then fires every `LevelTickEvent.Post`: mods' handlers,
 * most of a modded tick, keep running on the server thread, one level after another, and
 * events fired inside a level's tick are dispatched one at a time. A worker owns the level it
 * ticks meanwhile. Once out of levels, the server thread serves the chunk tasks of finished
 * levels with pending ones until the workers are done, so a worker reaching into another
 * level is not left waiting.
 *
 * Levels run in turn, exactly as without the patch, unless at least two took over 0.1 ms last
 * tick (below that, handing them to threads costs more than it saves; the limit is
 * `-Drava.parallelLevels.busyMicros`), and while the profiler is active or with
 * `-Drava.parallelLevels=false`.
 */
final class LevelTicker {
    private static final Field LEVEL_THREAD = field(Level.class, "thread");
    private static final Field CHUNKS_THREAD = field(ServerChunkCache.class, "mainThread");
    private static final Field CHUNK_TASKS = field(ServerChunkCache.class, "mainThreadProcessor");
    /** Off with `-Drava.parallelLevels=false`, or when this game version lacks a field. */
    private static final boolean ENABLED = !"false".equals(System.getProperty("rava.parallelLevels"))
            && LEVEL_THREAD != null && CHUNKS_THREAD != null && CHUNK_TASKS != null;
    /** A level whose last tick took longer is worth a thread of its own. */
    private static final long HEAVY_NANOS = 1000 * Long.getLong("rava.parallelLevels.busyMicros", 100);

    private static final Comparator<Job> SLOWEST_FIRST = Comparator.comparingLong(Job::lastNanos).reversed();

    private final Map<ServerLevel, Job> byLevel = new IdentityHashMap<>();
    private final List<Job> queued = new ArrayList<>();
    private final List<Job> order = new ArrayList<>();
    private Batch batch;
    private long ticks;
    private boolean reported;

    private static Field field(Class<?> owner, String name) {
        try {
            Field f = owner.getDeclaredField(name);
            f.setAccessible(true);
            return f;
        } catch (ReflectiveOperationException | RuntimeException e) {
            System.err.println("[parallel-levels] off: no " + owner.getName() + "." + name);
            return null;
        }
    }

    /** How many of `jobs` took longer than `HEAVY_NANOS` last tick. */
    private static int heavy(List<Job> jobs) {
        int n = 0;
        for (Job job : jobs) {
            if (job.lastNanos() > HEAVY_NANOS) {
                n++;
            }
        }
        return n;
    }

    void queue(ServerLevel level, BooleanSupplier hasTimeLeft) {
        Job job = byLevel.computeIfAbsent(level, Job::new);
        job.hasTimeLeft = hasTimeLeft;
        queued.add(job);
    }

    void run(ProfilerFiller profiler) {
        if (queued.isEmpty()) {
            return;
        }
        // Level times are taken once a second while levels run in turn, every tick otherwise.
        boolean parallel = ENABLED && heavy(queued) >= 2 && profiler == InactiveProfiler.INSTANCE;
        try {
            if (parallel) {
                runParallel(queued);
            } else {
                boolean timed = ++ticks % 20 == 0;
                for (Job job : queued) {
                    job.tick(timed);
                }
            }
        } finally {
            queued.clear();
        }
    }

    private void runParallel(List<Job> jobs) {
        if (!reported) {
            reported = true;
            System.err.println("[parallel-levels] " + jobs.size() + " levels, " + heavy(jobs) + " busy: ticking in parallel");
        }
        if (batch == null) {
            batch = new Batch(Math.max(1, Math.min(jobs.size(), Runtime.getRuntime().availableProcessors()) - 1));
        }
        for (Job job : jobs) {
            EventHooks.fireLevelTickPre(job.level, job.hasTimeLeft);
        }
        order.clear();
        order.addAll(jobs);
        order.sort(SLOWEST_FIRST);
        for (Job job : order) {
            job.done = false;
            job.failure = null;
        }
        EventLock.parallel(true);
        Throwable failure;
        try {
            failure = batch.run(order);
        } finally {
            EventLock.parallel(false);
        }
        if (failure instanceof ReportedException r) {
            throw r;
        }
        if (failure != null) {
            throw new ReportedException(CrashReport.forThrowable(failure, "Exception ticking world"));
        }
        for (Job job : jobs) {
            EventHooks.fireLevelTickPost(job.level, job.hasTimeLeft);
        }
    }

    /** A level's turn in the current tick. */
    static final class Job {
        final ServerLevel level;
        BooleanSupplier hasTimeLeft;
        /** How long the level's last timed `ServerLevel.tick` took. */
        long lastNanos;
        /** The level's chunk task queue, which the server thread serves once the level is done. */
        final BlockableEventLoop<?> chunkTasks;
        volatile boolean done;
        Throwable failure;

        Job(ServerLevel level) {
            this.level = level;
            BlockableEventLoop<?> tasks = null;
            try {
                tasks = ENABLED ? (BlockableEventLoop<?>) CHUNK_TASKS.get(level.getChunkSource()) : null;
            } catch (IllegalAccessException e) {
                // Not reached: the field was made accessible.
            }
            this.chunkTasks = tasks;
        }

        long lastNanos() {
            return lastNanos;
        }

        /** What `tickChildren` does for one level. */
        void tick(boolean timed) {
            EventHooks.fireLevelTickPre(level, hasTimeLeft);
            tickLevel(timed);
            EventHooks.fireLevelTickPost(level, hasTimeLeft);
        }

        /** `ServerLevel.tick`, with the crash report `tickChildren` makes. */
        void tickLevel(boolean timed) {
            long start = timed ? System.nanoTime() : 0;
            try {
                level.tick(hasTimeLeft);
            } catch (Throwable t) {
                CrashReport report = CrashReport.forThrowable(t, "Exception ticking world");
                level.fillReportDetails(report);
                throw new ReportedException(report);
            }
            if (timed) {
                lastNanos = System.nanoTime() - start;
            }
        }

        /** `tickLevel` on a worker, which owns the level meanwhile. */
        void tickOwned() throws IllegalAccessException {
            ServerChunkCache chunks = level.getChunkSource();
            Object levelThread = LEVEL_THREAD.get(level), chunksThread = CHUNKS_THREAD.get(chunks);
            Thread self = Thread.currentThread();
            LEVEL_THREAD.set(level, self);
            CHUNKS_THREAD.set(chunks, self);
            try {
                tickLevel(true);
            } finally {
                LEVEL_THREAD.set(level, levelThread);
                CHUNKS_THREAD.set(chunks, chunksThread);
            }
        }
    }
}
