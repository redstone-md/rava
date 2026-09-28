// IS THE DISPATCH LOOP PAYING FOR BRANCH MISPREDICTION?
//
// A `match op { .. }` interpreter has ONE indirect branch site for every opcode in the
// language, so the predictor sees a single jump whose target changes constantly. HotSpot's
// template interpreter re-dispatches at the END of each handler, giving every opcode its own
// branch site and its own predictor entry. If that difference is what costs rava ~17 cycles an
// opcode against HotSpot's ~2, then opcode VARIETY should cost measurably more than the same
// number of opcodes drawn from a small repeating set.
//
// Both loops execute the same number of int operations per iteration. SAME cycles through four
// distinct opcodes; VARIED spreads the identical work over many more. Same count, same types,
// different variety - so a per-opcode difference between them is the predictor, not the work.
public class DispatchVariety {
    static long same(int n) {
        int a = 0;
        for (int i = 0; i < n; i++) {
            a += 1; a += 1; a += 1; a += 1; a += 1; a += 1; a += 1; a += 1;
            a += 1; a += 1; a += 1; a += 1; a += 1; a += 1; a += 1; a += 1;
        }
        return a;
    }
    static long varied(int n) {
        int a = 0, b = 1, c = 2, d = 3;
        for (int i = 0; i < n; i++) {
            a += b; b -= c; c *= 2; d ^= a;
            a |= d; b &= 0x7fff; c <<= 1; d >>= 1;
            a -= c; b += d; c ^= b; d |= a;
            a &= 0xffff; b <<= 2; c >>>= 1; d *= 3;
        }
        return a + b + c + d;
    }
    static long run(int w, int n) {
        long t = System.nanoTime();
        if (w == 0) same(n); else varied(n);
        return System.nanoTime() - t;
    }
    public static void main(String[] s) {
        int n = 8_000_000, reps = 5;
        for (int w = 0; w < 3; w++) { run(0, n / 20); run(1, n / 20); }
        long[] best = {Long.MAX_VALUE, Long.MAX_VALUE};
        for (int r = 0; r < reps; r++)
            for (int w = 0; w < 2; w++) { long v = run(w, n); if (v < best[w]) best[w] = v; }
        // 16 statements per iteration in both; report ps per statement.
        System.out.println("SAME   PS_PER_OP=" + (best[0] * 1000 / (16L * n)));
        System.out.println("VARIED PS_PER_OP=" + (best[1] * 1000 / (16L * n)));
        System.out.println("HOTNS=" + (best[0] + best[1]));
    }
}
