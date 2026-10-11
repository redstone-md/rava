package net.minecraft.world.entity;

import net.minecraft.server.MinecraftServer;

/** Compile-time view of the members the patch opens; never packaged. */
public abstract class Entity {
    public PortalProcessor portalProcess;

    public void handlePortal() {
        throw new AssertionError();
    }

    public MinecraftServer getServer() {
        throw new AssertionError();
    }

    public boolean isRemoved() {
        throw new AssertionError();
    }
}
