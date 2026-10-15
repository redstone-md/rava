// Narrowing conversions feeding byte, char and short stores (fields, arrays), and float/double
// to int/long conversions with NaN, infinities and out-of-range values, compiled.
public class NarrowStoreSem {
    static final class P { byte b; short s; char c; boolean z; int i; long l; }

    static long run(int x, float f, double d, P p, byte[] ba, short[] sa, char[] ca) {
        p.b = (byte) x; p.s = (short) x; p.c = (char) x;
        p.b = (byte) (short) x;
        p.s = (short) (byte) x;
        p.c = (char) (byte) x;
        ba[x & 7] = (byte) (x >> 3); sa[x & 7] = (short) (char) x; ca[x & 7] = (char) (short) x;
        p.i = (int) f; p.l = (long) f;
        int di = (int) d; long dl = (long) d;
        return p.b * 31L + p.s * 7 + p.c + p.i + p.l + di + dl + ba[x & 7] + sa[x & 7] + ca[x & 7];
    }

    public static void main(String[] args) {
        P p = new P();
        byte[] ba = new byte[8]; short[] sa = new short[8]; char[] ca = new char[8];
        float[] fs = {0f, -1.5f, 3e9f, -3e9f, Float.NaN, Float.POSITIVE_INFINITY, Float.NEGATIVE_INFINITY, 1e19f, 123.75f};
        double[] ds = {0.0, 2.5e9, -2.5e9, Double.NaN, 1e300, -1e300, 9.3e18, -7.25};
        long h = 0;
        for (int i = 0; i < 200000; i++) h = h * 31 + run(i * 2654435761L > 0 ? i * 977 : -i * 131, fs[i % fs.length], ds[i % ds.length], p, ba, sa, ca);
        System.out.println(h + " " + p.b + " " + p.s + " " + (int) p.c);
    }
}
