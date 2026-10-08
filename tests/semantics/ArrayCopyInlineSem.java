// System.arraycopy into a fresh primitive array (compiled in place): every length from 0 to
// 40, char elements, a source aliasing the destination through a field, and each failure
// (null, negative, out of bounds, another array class) throwing as the interpreter does.
public class ArrayCopyInlineSem {
    static byte[] held;

    static long bytes(byte[] src, int sp, int dp, int len, int size) {
        byte[] d = new byte[size];
        System.arraycopy(src, sp, d, dp, len);
        long h = 0;
        for (byte b : d) h = h * 31 + b;
        return h;
    }

    static long chars(char[] src, int len) {
        char[] d = new char[len + 3];
        System.arraycopy(src, 1, d, 2, len);
        long h = 0;
        for (char c : d) h = h * 31 + c;
        return h;
    }

    static long fixed(byte[] src) {
        byte[] d = new byte[20];
        System.arraycopy(src, 0, d, 0, 1);
        System.arraycopy(src, 1, d, 1, 3);
        System.arraycopy(src, 4, d, 4, 7);
        System.arraycopy(src, 2, d, 11, 9);
        long h = 0;
        for (byte b : d) h = h * 31 + b;
        return h;
    }

    // The fresh array also reaches the copy as its own source, through memory.
    static long aliased(int n, int shift) {
        byte[] d = new byte[48];
        for (int i = 0; i < d.length; i++) d[i] = (byte) i;
        held = d;
        System.arraycopy(held, 0, d, shift, n);
        long h = 0;
        for (byte b : d) h = h * 31 + b;
        return h;
    }

    static String fail(Object src, int sp, int dp, int len) {
        try {
            byte[] d = new byte[8];
            System.arraycopy(src, sp, d, dp, len);
            return "ok " + d[0];
        } catch (RuntimeException e) {
            return e.getClass().getSimpleName();
        }
    }

    public static void main(String[] args) {
        byte[] src = new byte[64];
        for (int i = 0; i < src.length; i++) src[i] = (byte) (i * 7 + 3);
        char[] cs = new char[50];
        for (int i = 0; i < cs.length; i++) cs[i] = (char) (i * 131 + 7);
        long acc = 0;
        for (int r = 0; r < 3000; r++) {
            for (int len = 0; len <= 40; len++) acc += bytes(src, r % 5, r % 3, len, 48);
            acc += chars(cs, r % 40) + fixed(src) + aliased(r % 40, r % 7);
        }
        System.out.println("acc " + acc);
        for (int len = 0; len <= 40; len++) System.out.print(bytes(src, 3, 2, len, 48) % 1000 + " ");
        System.out.println();
        for (int n = 0; n <= 40; n += 3) System.out.print(aliased(n, 5) % 1000 + " ");
        System.out.println();
        System.out.println(fail(null, 0, 0, 1) + " " + fail(src, -1, 0, 1) + " " + fail(src, 0, -1, 1) + " " + fail(src, 0, 0, -1));
        System.out.println(fail(src, 60, 0, 8) + " " + fail(src, 0, 4, 5) + " " + fail(new int[4], 0, 0, 1) + " " + fail(src, 0, 0, 8));
    }
}
