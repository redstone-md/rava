// THE FAILURE THE GATES CANNOT SEE: a lambda entering ANOTHER SITE'S body.
//
// Every lambda of one capture signature shares a synthetic arena class id, so any cache keyed
// by CLASS rather than by SITE will hand one site's receiver another site's implementation.
// The four gates run fixed workloads whose hashes would still come out right if two lambdas of
// the same shape swapped bodies in a path they do not exercise; this probe is built to fail
// loudly if they ever do.
//
// Each group holds lambdas with IDENTICAL capture shapes and DIFFERENT bodies, called through
// one interface-typed variable so the call site is shared, and interleaved so no site stays
// monomorphic. Any body substitution changes the printed result.
import java.util.function.*;
public class LambdaSiteSem {
    static void p(String k, Object v) { System.out.println(k + " = " + v); }

    public static void main(String[] a) {
        // --- zero captures, same shape (IntUnaryOperator), four distinct bodies
        IntUnaryOperator[] z = { x -> x + 1, x -> x * 2, x -> x - 3, x -> x ^ 5 };
        StringBuilder zs = new StringBuilder();
        for (int r = 0; r < 3; r++) for (int i = 0; i < z.length; i++) zs.append(z[i].applyAsInt(10)).append(' ');
        p("zero-capture bodies", zs.toString().trim());

        // --- one capture each, same shape, different bodies AND different captured values
        int c1 = 100, c2 = 200;
        IntUnaryOperator[] o = { x -> x + c1, x -> x + c2, x -> x * c1, x -> x * c2 };
        StringBuilder os = new StringBuilder();
        for (int r = 0; r < 3; r++) for (int i = 0; i < o.length; i++) os.append(o[i].applyAsInt(3)).append(' ');
        p("one-capture bodies", os.toString().trim());

        // --- two captures, mixed types (int + ref), same shape
        String s1 = "A", s2 = "B";
        Function<String,String>[] f = new Function[]{
            (Function<String,String>) x -> s1 + x + c1,
            (Function<String,String>) x -> s2 + x + c1,
            (Function<String,String>) x -> s1 + x + c2,
            (Function<String,String>) x -> s2 + x + c2 };
        StringBuilder fs = new StringBuilder();
        for (int r = 0; r < 3; r++) for (int i = 0; i < f.length; i++) fs.append(f[i].apply("-")).append(' ');
        p("two-capture bodies", fs.toString().trim());

        // --- the same lambda EXPRESSION reached from two distinct call sites
        IntUnaryOperator g = x -> x * 7;
        int viaA = 0, viaB = 0;
        for (int i = 0; i < 5; i++) viaA += g.applyAsInt(i);        // site A
        for (int i = 0; i < 5; i++) viaB += g.applyAsInt(i);        // site B
        p("one lambda two sites", viaA + "/" + viaB);

        // --- capturing lambdas created in a LOOP: same site, many receivers, distinct captures
        IntUnaryOperator[] loop = new IntUnaryOperator[6];
        for (int i = 0; i < loop.length; i++) { final int k = i; loop[i] = x -> x + k * 1000; }
        StringBuilder ls = new StringBuilder();
        for (int r = 0; r < 3; r++) for (IntUnaryOperator u : loop) ls.append(u.applyAsInt(1)).append(' ');
        p("loop-made captures", ls.toString().trim());

        // --- void SAM (Consumer) and a wide capture (long), same shapes, different bodies
        long w1 = 1L << 40, w2 = 1L << 41;
        StringBuilder acc = new StringBuilder();
        Consumer<String>[] cs = new Consumer[]{
            (Consumer<String>) x -> acc.append(x).append(w1).append(';'),
            (Consumer<String>) x -> acc.append(x).append(w2).append(';') };
        for (int r = 0; r < 3; r++) for (Consumer<String> c : cs) c.accept("v");
        p("void-sam wide-capture", acc.toString());
    }
}
