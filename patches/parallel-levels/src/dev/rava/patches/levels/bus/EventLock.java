package dev.rava.patches.levels.bus;

import java.util.concurrent.locks.ReentrantLock;
import net.neoforged.bus.EventBus;
import net.neoforged.bus.api.Event;
import net.neoforged.bus.api.EventListener;

/**
 * Event dispatch while levels tick in parallel: one event at a time, so mods' handlers never
 * run concurrently. Defined by the event bus's loader (it names only bus and JDK classes) and
 * shared with the patch's level code, which turns it on and off.
 */
public final class EventLock {
    private static final ReentrantLock LOCK = new ReentrantLock();
    private static volatile boolean parallel;

    private EventLock() {}

    /** Whether levels are ticking in parallel now. */
    public static void parallel(boolean on) {
        parallel = on;
    }

    /** `EventBus.post(event, listeners)` in `EventBus.post`. */
    public static Event post(EventBus bus, Event event, EventListener[] listeners) {
        if (!parallel) {
            return bus.post(event, listeners);
        }
        LOCK.lock();
        try {
            return bus.post(event, listeners);
        } finally {
            LOCK.unlock();
        }
    }
}
