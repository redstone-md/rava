package net.neoforged.bus;

import net.neoforged.bus.api.Event;
import net.neoforged.bus.api.EventListener;

/** Compile-time view of the members the patch opens; never packaged. */
public class EventBus {
    public Event post(Event event, EventListener[] listeners) {
        throw new AssertionError();
    }
}
