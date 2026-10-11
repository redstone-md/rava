package dev.rava.patches.levels;

/** A worker ticking levels. */
final class LevelThread extends Thread {
    LevelThread(Runnable work, String name) {
        super(null, work, name, 16L << 20);
        setDaemon(true);
    }
}
