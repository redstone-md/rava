package java.util;

import java.util.function.Supplier;

/**
 * `java.util.Objects` as GUEST BYTECODE, replacing the native of the same name.
 *
 * <p>⭐**Why this class and not another.** The JIT-callback census puts `Objects.equals` at
 * 3.69% of all 64,088,346 compiled-to-host crossings in a boot, and the native layer's own
 * comment prices it at "17.3% of all native bridge self-time". Unlike the collection classes,
 * NOTHING here touches `$elems`/`$map` or the handle heap — which is exactly what made the
 * `java.util` collections experiment cost +8.46% — so this is the part of that idea that carries
 * none of its risk.
 *
 * <p>⭐**And it is MORE correct, not just faster.** `Objects.equals(a, b)` is
 * `(a == b) || (a != null && a.equals(b))`, i.e. it DISPATCHES to the guest's own `equals`. The
 * native could not do that: re-entering the interpreter from inside a native reached through
 * `rava_dispatch_miss` corrupted the operand-cell contract (four gates produced no hash at all,
 * with `java/lang/InternalError: jit dispatch` from a `StackUnderflow` in an unrelated
 * datafixers callee). The native therefore decides what it can from content keys it can derive
 * without running guest code. As bytecode there is no re-entry problem at all — an
 * `invokevirtual equals` is just an ordinary call the interpreter and the JIT already handle.
 */
public final class Objects {
    private Objects() {
        throw new AssertionError("No java.util.Objects instances for you!");
    }

    public static boolean equals(Object a, Object b) {
        return (a == b) || (a != null && a.equals(b));
    }

    /**
     * ⚠Self-contained on purpose. The real JDK delegates to the package-private
     * `Arrays.deepEquals0`, which does not exist on this engine's NATIVE `Arrays` — calling it
     * would compile against the real java.base and then die with an UnsatisfiedLinkError at
     * runtime. A patched class may only call APIs the engine actually serves.
     */
    public static boolean deepEquals(Object a, Object b) {
        if (a == b) {
            return true;
        }
        if (a == null || b == null) {
            return false;
        }
        // ⛔The first version handled only `Object[]` and fell through to `a.equals(b)` for a
        // primitive array, so `deepEquals(new int[]{1,2}, new int[]{1,2})` answered FALSE where
        // HotSpot answers true — array `equals` is identity. Caught by diffing against `java`
        // before this class was ever put on a classpath.
        if (a instanceof byte[] && b instanceof byte[]) {
            return Arrays.equals((byte[]) a, (byte[]) b);
        }
        if (a instanceof short[] && b instanceof short[]) {
            return Arrays.equals((short[]) a, (short[]) b);
        }
        if (a instanceof int[] && b instanceof int[]) {
            return Arrays.equals((int[]) a, (int[]) b);
        }
        if (a instanceof long[] && b instanceof long[]) {
            return Arrays.equals((long[]) a, (long[]) b);
        }
        if (a instanceof char[] && b instanceof char[]) {
            return Arrays.equals((char[]) a, (char[]) b);
        }
        if (a instanceof float[] && b instanceof float[]) {
            return Arrays.equals((float[]) a, (float[]) b);
        }
        if (a instanceof double[] && b instanceof double[]) {
            return Arrays.equals((double[]) a, (double[]) b);
        }
        if (a instanceof boolean[] && b instanceof boolean[]) {
            return Arrays.equals((boolean[]) a, (boolean[]) b);
        }
        if (a instanceof Object[] && b instanceof Object[]) {
            Object[] x = (Object[]) a;
            Object[] y = (Object[]) b;
            if (x.length != y.length) {
                return false;
            }
            for (int i = 0; i < x.length; i++) {
                if (!deepEquals(x[i], y[i])) {
                    return false;
                }
            }
            return true;
        }
        return a.equals(b);
    }

    public static int hashCode(Object o) {
        return o != null ? o.hashCode() : 0;
    }

    /** Inlined for the same reason as `deepEquals`: no dependency on the native `Arrays`. */
    public static int hash(Object... values) {
        if (values == null) {
            return 0;
        }
        int result = 1;
        for (Object element : values) {
            result = 31 * result + (element == null ? 0 : element.hashCode());
        }
        return result;
    }

    public static String toString(Object o) {
        return String.valueOf(o);
    }

    public static String toString(Object o, String nullDefault) {
        return (o != null) ? o.toString() : nullDefault;
    }

    public static <T> int compare(T a, T b, Comparator<? super T> c) {
        return (a == b) ? 0 : c.compare(a, b);
    }

    public static <T> T requireNonNull(T obj) {
        if (obj == null) {
            throw new NullPointerException();
        }
        return obj;
    }

    /**
     * ⚠The native builds the message with `str_of(args[1])`, which renders a null second
     * argument as the EMPTY string rather than propagating null — matched here so the 21
     * differential probes see no change of wording.
     */
    public static <T> T requireNonNull(T obj, String message) {
        if (obj == null) {
            throw new NullPointerException(message == null ? "" : message);
        }
        return obj;
    }

    public static <T> T requireNonNull(T obj, Supplier<String> messageSupplier) {
        if (obj == null) {
            throw new NullPointerException(messageSupplier == null ? null : messageSupplier.get());
        }
        return obj;
    }

    public static boolean isNull(Object obj) {
        return obj == null;
    }

    public static boolean nonNull(Object obj) {
        return obj != null;
    }

    public static <T> T requireNonNullElse(T obj, T defaultObj) {
        return (obj != null) ? obj : defaultObj;
    }

    public static <T> T requireNonNullElseGet(T obj, Supplier<? extends T> supplier) {
        return (obj != null) ? obj : supplier.get();
    }

    /** The JDK's own bounds primitive, and the source of the standard wording. */
    public static int checkIndex(int index, int length) {
        if (index < 0 || index >= length) {
            throw new IndexOutOfBoundsException("Index " + index + " out of bounds for length " + length);
        }
        return index;
    }

    public static int checkFromToIndex(int fromIndex, int toIndex, int length) {
        if (fromIndex < 0 || fromIndex > toIndex || toIndex > length) {
            throw new IndexOutOfBoundsException(
                "Range [" + fromIndex + ", " + toIndex + ") out of bounds for length " + length);
        }
        return fromIndex;
    }

    public static int checkFromIndexSize(int fromIndex, int size, int length) {
        if ((length | fromIndex | size) < 0 || size > length - fromIndex) {
            throw new IndexOutOfBoundsException(
                "Range [" + fromIndex + ", " + fromIndex + " + " + size + ") out of bounds for length " + length);
        }
        return fromIndex;
    }

    public static int toIntExact(long value) {
        if ((int) value != value) {
            throw new ArithmeticException("integer overflow");
        }
        return (int) value;
    }
}
