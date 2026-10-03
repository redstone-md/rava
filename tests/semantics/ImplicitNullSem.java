// Null objects reaching optimized code whose null checks fault instead of testing: field loads
// and stores, float fields, array lengths and indexed loads. Each lambda class runs hot with a
// non-null capture first (its body compiled with the access), then gets null and must throw
// the NullPointerException the interpreter would, and keep working afterwards.
import java.util.function.IntSupplier;

public class ImplicitNullSem {
    static final class Box { int i; long l; double d; Box next; int[] arr; }
    static IntSupplier load(Box b) { return () -> (int) (b.i + b.l); }
    static IntSupplier fload(Box b) { return () -> (int) (b.d * 2); }
    static IntSupplier store(Box b) { return () -> { b.i = 3; return 0; }; }
    static IntSupplier len(int[] a) { return () -> a.length; }
    static IntSupplier at(int[] a) { return () -> a[1]; }
    static IntSupplier chain(Box b) { return () -> b.next.arr.length; }
    static int call(IntSupplier f) {
        try { return f.getAsInt(); } catch (NullPointerException e) { return -1000; }
    }
    public static void main(String[] args) {
        Box b = new Box(); b.i = 3; b.l = 4; b.d = 1.5; b.arr = new int[] { 7, 8, 9 }; b.next = b;
        IntSupplier[] good = { load(b), fload(b), store(b), len(b.arr), at(b.arr), chain(b) };
        IntSupplier[] bad = { load(null), fload(null), store(null), len(null), at(null), chain(new Box()) };
        long acc = 0;
        for (int r = 0; r < 300000; r++) acc += call(good[r % good.length]);
        System.out.println(acc);
        for (int k = 0; k < bad.length; k++) System.out.print(call(bad[k]) + " ");
        System.out.println();
        for (int r = 0; r < 300000; r++) acc += call(good[r % good.length]) + (r % 1000 == 0 ? call(bad[r % bad.length]) : 0);
        System.out.println(acc);
    }
}
