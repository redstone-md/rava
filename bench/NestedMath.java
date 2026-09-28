// Nested pure-numeric calls: hot(n) accumulates sq(i) in a loop. Both methods are
// all-numeric static → LLVM-eligible. The whole-tree LLVM module emits hot + sq as one
// unit with a direct internal call, so clang -O2 -march=x86-64-v3 inlines sq into hot and
// vectorises across. SUM must be bit-exact vs the reference (i32 index widened, i64 accumulate).
public class NestedMath {
    static long sq(long x) {
        return x * x;
    }

    static long hot(int n) {
        long s = 0;
        for (int i = 0; i < n; i++) {
            s += sq(i);
        }
        return s;
    }

    public static void main(String[] a) {
        int n = a.length > 0 ? Integer.parseInt(a[0]) : 100_000_000;
        for (int w = 0; w < 8; w++) hot(n); // warm the JIT / AOT tier
        long t0 = System.nanoTime();
        long s = hot(n);
        System.out.println("SUM=" + s + " HOTNS=" + (System.nanoTime() - t0));
    }
}
