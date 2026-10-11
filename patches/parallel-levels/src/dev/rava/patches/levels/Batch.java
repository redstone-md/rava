package dev.rava.patches.levels;

import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicLong;
import java.util.concurrent.locks.LockSupport;

/**
 * The server thread and its workers taking the jobs of one tick from a shared counter,
 * stamped with the tick's generation so a late worker never takes a job of another tick.
 * Workers spin briefly for the next tick's jobs, then park.
 */
final class Batch {
    private static final int SPIN = 5_000;

    private final Thread[] workers;
    private final Thread server = Thread.currentThread();
    /** Generation << 32 | index of the next job. */
    private final AtomicLong state = new AtomicLong();
    private final AtomicInteger finished = new AtomicInteger();
    private volatile List<LevelTicker.Job> jobs;

    Batch(int n) {
        workers = new Thread[n];
        for (int i = 0; i < n; i++) {
            workers[i] = new LevelThread(this::work, "Level thread " + (i + 1));
            workers[i].setDaemon(true);
            workers[i].start();
        }
    }

    /** Ticks the levels of `jobs` (heaviest first); returns the first failure. */
    Throwable run(List<LevelTicker.Job> jobs) {
        this.jobs = jobs;
        finished.set(0);
        long generation = (state.get() >>> 32) + 1;
        state.set(generation << 32);
        for (Thread w : workers) {
            LockSupport.unpark(w);
        }
        drain(generation, jobs, false);
        while (finished.get() < jobs.size()) {
            boolean served = false;
            for (LevelTicker.Job job : jobs) {
                if (job.done && job.chunkTasks.getPendingTasksCount() > 0 && job.level.getChunkSource().pollTask()) {
                    served = true;
                }
            }
            if (!served) {
                LockSupport.parkNanos(50_000);
            }
        }
        Throwable failure = null;
        for (LevelTicker.Job job : jobs) {
            if (job.failure != null && failure == null) {
                failure = job.failure;
            }
        }
        return failure;
    }

    /** Takes and ticks jobs of `generation` until none is left. */
    private void drain(long generation, List<LevelTicker.Job> list, boolean worker) {
        while (true) {
            long s = state.get();
            if (s >>> 32 != generation || (int) s >= list.size()) {
                return;
            }
            if (!state.compareAndSet(s, s + 1)) {
                continue;
            }
            LevelTicker.Job job = list.get((int) s);
            try {
                if (worker) {
                    job.tickOwned();
                } else {
                    job.tickLevel(true);
                }
            } catch (Throwable t) {
                job.failure = t;
            }
            job.done = true;
            finished.incrementAndGet();
            if (worker) {
                LockSupport.unpark(server);
            }
        }
    }

    private void work() {
        long seen = 0;
        while (true) {
            for (int spins = 0; state.get() >>> 32 == seen; spins++) {
                if (spins < SPIN) {
                    Thread.onSpinWait();
                } else {
                    LockSupport.park(this);
                }
            }
            seen = state.get() >>> 32;
            drain(seen, jobs, true);
        }
    }
}
