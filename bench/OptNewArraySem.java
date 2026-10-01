/** Arrays allocated by optimized code: constant and variable lengths of every element size,
 *  around the 256-byte limit of the inline variable-length path, zero length, negative length
 *  (NegativeArraySizeException), and enough of them to cross many bump runs and collections. */
public class OptNewArraySem {
    static long fill(int n) {
        byte[] b = new byte[n];
        short[] s = new short[n];
        int[] i = new int[n];
        long[] l = new long[n];
        double[] d = new double[n];
        Object[] o = new Object[n];
        long sum = b.length + s.length + i.length + l.length + d.length + o.length;
        for (int k = 0; k < n; k++) {
            sum += b[k] + s[k] + i[k] + l[k] + (long) d[k] + (o[k] == null ? 1 : 0);
            b[k] = (byte) k; i[k] = k; l[k] = k; o[k] = b;
        }
        for (int k = 0; k < n; k++) sum += b[k] + i[k] + l[k] + (o[k] == b ? 1 : 0);
        return sum;
    }

    static long constants() {
        int[] a = new int[3];
        double[] d = new double[16];
        byte[] big = new byte[5000];
        long[] z = new long[0];
        a[2] = 7; d[15] = 1.5; big[4999] = 9;
        return a[0] + a[2] + (long) d[15] + big[4999] + big[0] + z.length + a.length + d.length + big.length;
    }

    public static void main(String[] args) {
        long sum = 0;
        int[] sizes = {0, 1, 2, 7, 8, 15, 16, 28, 29, 30, 57, 58, 59, 115, 116, 117, 231, 232, 233, 300, 1000};
        for (int r = 0; r < 3000; r++) {
            for (int n : sizes) sum += fill(n);
            sum += constants();
        }
        int bad = 0;
        for (int r = 0; r < 2000; r++) {
            try { sum += fill(-1 - (r & 3)); } catch (NegativeArraySizeException e) { bad++; }
        }
        System.out.println(sum + " " + bad);
    }
}
