// Off-heap memory through sun.misc.Unsafe with a null base (lwjgl's memGetInt/memPutInt), as
// optimized code addresses it: `p + c`, `p + (i << k)` and `p + i` taken as the address with no
// add, in loops the compiler unrolls (constant indexes) and in ones it does not, for each
// primitive width, with stores read back through other widths and overlapping ranges.
import java.lang.reflect.Field;
import sun.misc.Unsafe;

public class OffHeapSem {
    static final Unsafe U;
    static {
        try {
            Field f = Unsafe.class.getDeclaredField("theUnsafe");
            f.setAccessible(true);
            U = (Unsafe) f.get(null);
        } catch (Exception e) {
            throw new RuntimeException(e);
        }
    }
    static final int COUNT = 7;

    static int getInt(long p) { return U.getInt(null, p); }
    static void putInt(long p, int v) { U.putInt(null, p, v); }

    static long scaled(long p, long q, int n, int salt) {
        long s = 0;
        for (int i = 0; i < n; i++) {
            putInt(q + ((long) i << 2), getInt(p + 8 + i * 8L) ^ salt);
            U.putLong(null, q + 512 + ((long) i << 3), U.getLong(null, p + ((long) i << 3)) + i);
            U.putShort(null, q + 1024 + ((long) i << 1), (short) (U.getShort(null, p + i * 2L) * 3));
            U.putByte(null, q + 1536 + i, (byte) (U.getByte(null, p + i) - 7));
            s += U.getInt(null, q + ((long) i << 2)) + U.getLong(null, q + 512 + ((long) i << 3));
        }
        return s;
    }

    static long unrolled(long p, long q, int mask) {
        int size = 0;
        for (int i = 0; i < COUNT; i++) {
            putInt(q + ((long) size << 2), getInt(p + 8 + i * 8L));
            putInt(q + 64 + ((long) size << 2), getInt(p + 12 + i * 8L));
            U.putDouble(null, q + 128 + i * 8L, U.getDouble(null, p + 128 + i * 8L) * 0.5);
            U.putFloat(null, q + 256 + i * 4L, U.getFloat(null, p + 256 + i * 4L) + 1);
            size += (mask >> i) & 1;
        }
        long s = size;
        for (int i = 0; i < 16; i++) {
            s = s * 31 + getInt(q + i * 4L) + (long) U.getDouble(null, q + 128 + (i & 7) * 8L);
        }
        return s;
    }

    static long base(long p, long off, long idx) {
        // `p + idx` (two variables, scale 1) and `(idx << 2) + p` (the shift on the left).
        U.putInt(null, p + idx, 0x01020304);
        int a = U.getInt(null, p + idx);
        int b = U.getByte(null, p + idx + 1);
        U.putInt(null, (idx << 2) + p, a + b);
        return a + b + U.getInt(null, (idx << 2) + p) + U.getShort(null, p + off);
    }

    public static void main(String[] args) {
        long p = U.allocateMemory(4096), q = U.allocateMemory(4096);
        for (int i = 0; i < 1024; i++) U.putInt(p + i * 4L, i * 0x9E3779B1);
        for (int i = 0; i < 1024; i++) U.putInt(q + i * 4L, 0);
        long s1 = 0, s2 = 0, s3 = 0;
        for (int r = 0; r < 20000; r++) {
            s1 += scaled(p, q, 64, r);
            s2 += unrolled(p, q, r & 0x7f);
            s3 += base(q + 2048, r & 15, r & 31);
        }
        System.out.println("scaled " + s1);
        System.out.println("unrolled " + s2);
        System.out.println("base " + s3);
        int h = 0;
        for (int i = 0; i < 1024; i++) h = h * 31 + U.getInt(q + i * 4L);
        System.out.println("memory " + h);
        U.freeMemory(p);
        U.freeMemory(q);
    }
}
