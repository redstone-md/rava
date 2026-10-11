package dev.rava.patches.levels;

import net.minecraft.world.entity.Entity;

/**
 * `Entity.handlePortal` in `Entity.baseTick`: on a level thread, an entity in a portal is
 * handled by the server thread after the tick instead, since going through reaches into the
 * destination level (finding or building its portal, adding the entity).
 */
public final class Portals {
    private Portals() {}

    public static void handlePortal(Entity entity) {
        if (entity.portalProcess == null || !(Thread.currentThread() instanceof LevelThread)) {
            entity.handlePortal();
            return;
        }
        entity.getServer().execute(() -> {
            if (!entity.isRemoved()) {
                entity.handlePortal();
            }
        });
    }
}
