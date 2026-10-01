// Repeated unresolved native calls must keep throwing, including from compiled callers.
public class NativeDeclineSem {
    static int invoke(NativeDeclineTarget target, int value) {
        return target.unimplemented(value);
    }

    public static void main(String[] args) {
        NativeDeclineTarget target = new NativeDeclineTarget();
        int failures = 0;
        int unexpected = 0;
        for (int i = 0; i < 512; i++) {
            try {
                unexpected += invoke(target, i) + 1;
            } catch (UnsatisfiedLinkError expected) {
                failures++;
            }
        }
        System.out.println("native-decline " + failures + " " + unexpected);
    }
}

class NativeDeclineTarget {
    native int unimplemented(int value);
}
