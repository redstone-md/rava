// A rejected compiled cast must throw before the following observable store.
// Existing interpreter policy treats Object[] element compatibility as unknown
// because native-created arrays can lose their precise component descriptor.
// This line records that known gap; unrelated array types must still reject.
// DIVERGES: erased-array=
public class JitCastSem {
    interface Marker {}
    static class Target implements Marker { int value = 42; }
    static int after;
    static Target checked(Object value) {
        Target result = (Target) value;
        after++;
        return result;
    }
    static Marker marker(Object value) { return (Marker) value; }
    static String[] strings(Object value) { return (String[]) value; }
    public static void main(String[] args) {
        Target target = new Target();
        int rejected = 0;
        for (int i = 0; i < 4096; i++) {
            target = new Target();
            if (checked(target) != target) throw new AssertionError("identity");
            try { checked("wrong"); }
            catch (ClassCastException expected) { rejected++; }
            if (target.value != 42) throw new AssertionError("live reference");
        }
        System.out.println("rejected=" + rejected);
        System.out.println("after=" + after);
        System.out.println("null=" + (checked(null) == null));
        System.out.println("interface=" + (marker(target) == target));
        System.out.println("array=" + strings(new String[]{"ok"})[0]);
        int[] primitive = {1};
        int arrayRejected = 0;
        for (int i = 0; i < 4096; i++) {
            try { strings(primitive); }
            catch (ClassCastException expected) { arrayRejected++; }
        }
        System.out.println("bad-array=" + arrayRejected);
        try { strings(new Object[]{"wrong"}); System.out.println("erased-array=miss"); }
        catch (ClassCastException expected) { System.out.println("erased-array=throw"); }
    }
}
