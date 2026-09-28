// Generic field-access hot path — getfield/putfield on an object with many
// fields. The JIT reads a field by SCANNING the object's interned-id array, so a
// 16-field object pays a linear scan per access. Isolates that cost so the
// field-slot-hint optimization (Stage-2 B) is measurable deterministically.
public class Fields {
    static final class Obj {
        int a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p;
        Obj() {
            a = 1; b = 2; c = 3; d = 4; e = 5; f = 6; g = 7; h = 8;
            i = 9; j = 10; k = 11; l = 12; m = 13; n = 14; o = 15; p = 16;
        }
    }

    // Reads fields spread across the layout (forcing deep id scans) and writes a
    // couple back. No invokes/allocs in the loop → the JIT compiles it, so this
    // measures COMPILED field access.
    static long hot(int iters, Obj q) {
        long acc = 0;
        for (int x = 0; x < iters; x++) {
            int s = q.a + q.h + q.p + q.l + q.d + q.o;
            q.p = s & 0xFFFF;
            q.h = q.h + 1;
            acc += s;
        }
        return acc;
    }

    public static void main(String[] args) {
        for (int w = 0; w < 15; w++) hot(1_000_000, new Obj()); // warmup → compile
        Obj q = new Obj();
        long t0 = System.nanoTime();
        long s = hot(20_000_000, q);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + s);
        System.out.println("HOTNS=" + ns);
    }
}
