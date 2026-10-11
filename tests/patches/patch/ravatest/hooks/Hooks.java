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

    /** Reads and rewrites `Box.v`, which the patch opens (no setAccessible needed), and the
     *  field `tag` the patch adds. */
    public static String peek(Object box) {
        try {
            Field f = box.getClass().getDeclaredField("v");
            int was = f.getInt(box);
            f.setInt(box, 99);
            // `tag` is a field the patch adds to Box.
            Field tag = box.getClass().getField("tag");
            tag.setLong(box, tag.getLong(box) + 5);
            return "peeked " + was + " tag " + tag.getLong(box);
        } catch (ReflectiveOperationException e) {
            return "failed: " + e;
        }
    }
}
