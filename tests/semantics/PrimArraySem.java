// Differential probe for structurally confined primitive arrays.
// Each leaf is invoked past the normal JIT threshold before its checked result.
public class PrimArraySem {
    static int marker;

    static boolean boolCase(boolean value) {
        boolean[] a = new boolean[1];
        a[0] = value;
        return a[0];
    }

    static int byteCase(int value) {
        byte[] a = new byte[1];
        a[0] = (byte) value;
        return a[0];
    }

    static int charCase(int value) {
        char[] a = new char[1];
        a[0] = (char) value;
        return a[0];
    }

    static int shortCase(int value) {
        short[] a = new short[1];
        a[0] = (short) value;
        return a[0];
    }

    static int intCase(int value) {
        int[] a = new int[1];
        a[0] = value;
        return a[0];
    }

    static long longCase(long value) {
        long[] a = new long[1];
        a[0] = value;
        return a[0];
    }

    static float floatCase(float value) {
        float[] a = new float[1];
        a[0] = value;
        return a[0];
    }

    static double doubleCase(double value) {
        double[] a = new double[1];
        a[0] = value;
        return a[0];
    }

    static int storeAt(int index) {
        marker = 1;
        int[] a = new int[1];
        a[index] = 7;
        a = null;
        marker = 2;
        return 7;
    }

    static int loadAt(int index) {
        marker = 1;
        int[] a = new int[1];
        a[0] = 9;
        int value = a[index];
        a = null;
        marker = 2;
        return value;
    }

    static String storeFault() {
        marker = 0;
        try {
            storeAt(2);
            return "NO THROW:" + marker;
        } catch (Throwable t) {
            return t.getClass().getName() + ":" + marker;
        }
    }

    static String loadFault() {
        marker = 0;
        try {
            loadAt(2);
            return "NO THROW:" + marker;
        } catch (Throwable t) {
            return t.getClass().getName() + ":" + marker;
        }
    }

    public static void main(String[] args) {
        long warm = 0;
        for (int i = 0; i < 3000; i++) {
            warm += boolCase((i & 1) != 0) ? 1 : 0;
            warm += byteCase(i);
            warm += charCase(i);
            warm += shortCase(i);
            warm += intCase(i);
            warm += longCase(i);
            warm += Float.floatToRawIntBits(floatCase(i + 0.25f));
            warm += Double.doubleToRawLongBits(doubleCase(i + 0.5));
            warm += storeAt(0) + loadAt(0);
        }

        System.out.println("warm " + warm);
        System.out.println("boolean " + boolCase(true) + " " + boolCase(false));
        System.out.println("byte " + byteCase(0x1ff) + " " + byteCase(0x180));
        System.out.println("char " + charCase(0x1ffff));
        System.out.println("short " + shortCase(0xffff) + " " + shortCase(0x8000));
        System.out.println("int " + intCase(0x89abcdef));
        System.out.println("long " + longCase(0x123456789abcdef0L));
        System.out.println("float " + Float.floatToRawIntBits(floatCase(-13.25f)));
        System.out.println("double " + Double.doubleToRawLongBits(doubleCase(-17.5)));
        System.out.println("store.oob " + storeFault());
        System.out.println("load.oob " + loadFault());
    }
}
