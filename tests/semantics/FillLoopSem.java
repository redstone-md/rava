/** Fill loops (`for (i = from; i < to; i++) a[i] = x`) the optimizing tier turns into vector
 *  stores: every element type it takes, ranges from empty to past several vectors, starting
 *  at odd offsets, with neighbours that must stay untouched, and the counter read after. */
public class FillLoopSem {
    static int fillInt(int[] a, int from, int to, int x) {
        int i;
        for (i = from; i < to; i++) a[i] = x;
        return i;
    }
    static void fillLong(long[] a, int from, int to, long x) {
        for (int i = from; i < to; i++) a[i] = x;
    }
    static void fillFloat(float[] a, int from, int to, float x) {
        for (int i = from; i < to; i++) a[i] = x;
    }
    static void fillDouble(double[] a, int from, int to, double x) {
        for (int i = from; i < to; i++) a[i] = x;
    }
    static int fillByte(byte[] a, int from, int to, byte x) {
        int i;
        for (i = from; i < to; i++) a[i] = x;
        return i;
    }
    static int fillBoolean(boolean[] a, int from, int to, boolean x) {
        int i;
        for (i = from; i < to; i++) a[i] = x;
        return i;
    }
    static int fillShort(short[] a, int from, int to, short x) {
        int i;
        for (i = from; i < to; i++) a[i] = x;
        return i;
    }
    static int fillChar(char[] a, int from, int to, char x) {
        int i;
        for (i = from; i < to; i++) a[i] = x;
        return i;
    }
    static void whole(int[] a) {
        for (int i = 0; i < a.length; i++) a[i] = -1;
    }

    public static void main(String[] args) {
        long h = 0;
        for (int round = 0; round < 3000; round++) {
            for (int n = 0; n <= 37; n++) {
                int from = round % 5, to = Math.min(from + n, 40);
                int[] ia = new int[42];
                byte[] ba = new byte[42];
                boolean[] za = new boolean[42];
                short[] sa = new short[42];
                char[] ca = new char[42];
                int end = fillInt(ia, from, to, round * 31 + n);
                byte bx = (byte) (round * 31 + n * 0x8001 + 0x7fffff80);
                boolean zx = (bx & 1) != 0;
                short sx = (short) (round * 31 + n * 0x8001 + 0x7fff8000);
                char cx = (char) (round * 31 + n * 0x8001 + 0xffff8000);
                int be = fillByte(ba, from, to, bx);
                int ze = fillBoolean(za, from, to, zx);
                int se = fillShort(sa, from, to, sx);
                int ce = fillChar(ca, from, to, cx);
                long[] la = new long[42];
                fillLong(la, from, to, (long) round << 33 | n);
                float[] fa = new float[42];
                fillFloat(fa, from, to, round + n * 0.5f);
                double[] da = new double[42];
                fillDouble(da, from, to, round - n * 0.25);
                int[] wa = new int[n];
                whole(wa);
                for (int k = 0; k < 42; k++) h = h * 31 + ia[k] + ba[k] + (za[k] ? 1 : 0) + sa[k] + ca[k] + la[k] + Float.floatToIntBits(fa[k]) + Double.doubleToLongBits(da[k]);
                for (int k = 0; k < n; k++) h = h * 7 + wa[k];
                h += end + be + ze + se + ce;
            }
        }
        System.out.println(h);
        try {
            fillInt(new int[4], 2, 9, 1);
            System.out.println("no exception");
        } catch (ArrayIndexOutOfBoundsException e) {
            System.out.println("AIOOBE " + e.getMessage());
        }
        byte[] partial = {9, 9, 9, 9};
        try {
            fillByte(partial, 1, 6, (byte) 0xff);
            System.out.println("no byte exception");
        } catch (ArrayIndexOutOfBoundsException e) {
            System.out.println("byte AIOOBE " + java.util.Arrays.toString(partial));
        }
        short[] partialShort = {9, 9, 9};
        try {
            fillShort(partialShort, 1, 4, (short) 0x8001);
            System.out.println("no short exception");
        } catch (ArrayIndexOutOfBoundsException e) {
            System.out.println("short AIOOBE " + java.util.Arrays.toString(partialShort));
        }
        char[] partialChar = {1, 2, 3};
        try {
            fillChar(partialChar, 1, 4, (char) 0xf001);
            System.out.println("no char exception");
        } catch (ArrayIndexOutOfBoundsException e) {
            System.out.println("char AIOOBE " + java.util.Arrays.toString(partialChar));
        }
        System.out.println("byte null/empty " + fillByte(null, 2, 2, (byte) -1));
        System.out.println("boolean null/empty " + fillBoolean(null, 2, 2, true));
        try {
            fillByte(null, 0, 1, (byte) 1);
            System.out.println("no byte NPE");
        } catch (NullPointerException e) {
            System.out.println("byte NPE");
        }
        try {
            fillBoolean(null, 0, 1, true);
            System.out.println("no boolean NPE");
        } catch (NullPointerException e) {
            System.out.println("boolean NPE");
        }
        try {
            fillShort(null, 0, 1, (short) 0x8001);
            System.out.println("no short NPE");
        } catch (NullPointerException e) {
            System.out.println("short NPE");
        }
        try {
            fillChar(null, 0, 1, (char) 0xf001);
            System.out.println("no char NPE");
        } catch (NullPointerException e) {
            System.out.println("char NPE");
        }
    }
}
