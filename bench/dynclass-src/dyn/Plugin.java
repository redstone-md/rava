package dyn;

import probe.DynApi;
import probe.DynBase;

public class Plugin extends DynBase implements DynApi {
    public static int VALUE = seed();

    public static int seed() {
        return 19;
    }

    @Override
    public int value() {
        return 7;
    }

    public int add(int x) {
        return value() + x;
    }

    @Override
    public void fail() {
        throw new IllegalArgumentException("dynamic-boom");
    }
}
