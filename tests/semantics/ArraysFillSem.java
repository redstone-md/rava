// Arrays.fill of every primitive array type, with values whose bits test the element width
// (negative bytes, high chars, NaN and negative zero), a null array, and an empty one; run
// interpreted and again once the loop is compiled (the optimizing tier's in-place routine).
import java.util.Arrays;

public class ArraysFillSem {
    static String run(int k) {
        byte[] b = new byte[7 + k % 3];
        boolean[] z = new boolean[5];
        char[] c = new char[9];
        short[] s = new short[3];
        int[] i = new int[11];
        float[] f = new float[4];
        long[] j = new long[6];
        double[] d = new double[2];
        Arrays.fill(b, (byte) -3);
        Arrays.fill(z, true);
        Arrays.fill(c, '￮');
        Arrays.fill(s, (short) -2);
        Arrays.fill(i, 0x80000001);
        Arrays.fill(f, Float.NaN);
        Arrays.fill(j, 0x8000000000000007L);
        Arrays.fill(d, -0.0);
        Arrays.fill(new int[0], 5);
        b[1] = 1;
        String npe;
        try {
            Arrays.fill((long[]) null, 1L);
            npe = "no exception";
        } catch (NullPointerException e) {
            npe = "npe";
        }
        return Arrays.toString(b) + Arrays.toString(z) + (int) c[8] + Arrays.toString(s) + i[10] + Float.floatToRawIntBits(f[3]) + j[5] + Double.doubleToRawLongBits(d[1]) + " " + npe;
    }

    public static void main(String[] args) {
        System.out.println(run(0));
        String last = null;
        for (int k = 0; k < 200_000; k++) last = run(k);
        System.out.println(last);
        // A large fill overwriting old values entirely.
        byte[] big = new byte[1 << 22];
        for (int r = 0; r < 50; r++) {
            big[r * 7919] = 9;
            Arrays.fill(big, (byte) r);
        }
        long sum = 0;
        for (byte x : big) sum += x;
        System.out.println(sum);
    }
}
