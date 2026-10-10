package ravatest.hooks;

import java.lang.reflect.Field;
import ravatest.app.PatchApp;

/** The test patch's hooks: each replaces one call in `PatchApp.run`. */
public final class Hooks {
    private Hooks() {}

    public static String greet(PatchApp app, String who) {
        return app.greet(who) + " (patched)";
    }

    public static int twice(int x) {
        return 3 * x;
    }

    public static int area(PatchApp.Shape s) {
        return s.area() + Counter.next();
    }

    /** Reads and rewrites `Box.v`, which the patch opens: no setAccessible needed. */
    public static String peek(Object box) {
        try {
            Field f = box.getClass().getDeclaredField("v");
            int was = f.getInt(box);
            f.setInt(box, 99);
            return "peeked " + was;
        } catch (ReflectiveOperationException e) {
            return "failed: " + e;
        }
    }
}
