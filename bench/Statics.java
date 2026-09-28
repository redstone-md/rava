// getstatic hot path — repeated reads of static fields. Non-final so the javac
// can't fold them to constants (real getstatic ops). The loop never writes a
// static, so rava's generation-guarded getstatic cache stays valid → hits.
public class Statics {
    static int A = 1, B = 2, C = 3, D = 4, E = 5, F = 6, G = 7, H = 8;

    static long hot(int iters) {
        long acc = 0;
        for (int i = 0; i < iters; i++) {
            acc += A + B + C + D + E + F + G + H; // 8 getstatic per iter
        }
        return acc;
    }

    public static void main(String[] args) {
        for (int w = 0; w < 15; w++) hot(1_000_000); // warmup → compile
        long t0 = System.nanoTime();
        long s = hot(20_000_000);
        long ns = System.nanoTime() - t0;
        System.out.println("SUM=" + s);
        System.out.println("HOTNS=" + ns);
    }
}
