// THE REST OF THE SEAM CENSUS. 64,088,346 compiled-to-host crossings a boot at ~700 ns average
// is 44.9 s — the whole wall. The top-20 callees are ~70% of that population, so each one priced
// against HotSpot is a named piece of the boot.
//
// ⚠Each loop in its own method, called repeatedly, so it reaches the compiled tier by entry count.
import java.util.*;
import java.util.function.*;
import it.unimi.dsi.fastutil.doubles.*;

public class SeamCost2 {
    static double[] darr = new double[8];
    static List<String> list = new ArrayList<>();
    static Set<String> set = new HashSet<>();
    static Map<String,String> hm = new HashMap<>();
    static Function<String,String> fn = s -> s;
    static IntPredicate ip = i -> i > 0;
    static Supplier<String> sup = () -> "s";
    static java.util.function.BiFunction<String,String,String> bifn = (x,y) -> x;
    static java.util.function.UnaryOperator<String> uop = x -> x;
    static Object obj = new Object();
    static Function<Object,Object> ofn = x -> x;
    static Object o1 = new Object(), o2 = new Object();
    static String[] sarr = new String[8];
    static StringBuilder sb = new StringBuilder();

    static long wrap(int n)      { long a = 0; for (int i = 0; i < n; i++) a += DoubleArrayList.wrap(darr).size(); return a; }
    static long fnApply(int n)   { long a = 0; for (int i = 0; i < n; i++) a += fn.apply("x") == null ? 0 : 1; return a; }
    static long objEq(int n)     { long a = 0; for (int i = 0; i < n; i++) a += Objects.equals(o1, o2) ? 1 : 0; return a; }
    static long strValueOf(int n){ long a = 0; for (int i = 0; i < n; i++) a += String.valueOf(i).length(); return a; }
    static long setIter(int n)   { long a = 0; for (int i = 0; i < n; i++) for (String s : set) a += s.length(); return a; }
    static long arrClone(int n)  { long a = 0; for (int i = 0; i < n; i++) a += sarr.clone().length; return a; }
    static long appendOp(int n)  { long a = 0; for (int i = 0; i < n; i++) { sb.setLength(0); sb.append("ab"); a += sb.length(); } return a; }
    static long reqNonNull(int n){ long a = 0; for (int i = 0; i < n; i++) a += Objects.requireNonNull(o1) == null ? 0 : 1; return a; }
    static long idHash(int n)    { long a = 0; for (int i = 0; i < n; i++) a += System.identityHashCode(o1) & 1; return a; }
    static long listAdd(int n)   { long a = 0; for (int i = 0; i < n; i++) { list.clear(); list.add("x"); a += list.size(); } return a; }
    static long ipTest(int n)    { long a = 0; for (int i = 0; i < n; i++) a += ip.test(i) ? 1 : 0; return a; }
    static long ofnApply(int n) { long a = 0; for (int i = 0; i < n; i++) a += ofn.apply(obj) == null ? 0 : 1; return a; }
    static long supGet(int n)   { long a = 0; for (int i = 0; i < n; i++) a += sup.get() == null ? 0 : 1; return a; }
    static long biApply(int n)  { long a = 0; for (int i = 0; i < n; i++) a += bifn.apply("x","y") == null ? 0 : 1; return a; }
    static long uopApply(int n) { long a = 0; for (int i = 0; i < n; i++) a += uop.apply("x") == null ? 0 : 1; return a; }
    static long ctl(int n)       { long a = 0; for (int i = 0; i < n; i++) a += i & 1; return a; }

    static long time(String nm, int w, int n, int reps) {
        long best = Long.MAX_VALUE, acc = 0;
        for (int r = 0; r < reps; r++) {
            long t0 = System.nanoTime();
            long v = switch (w) {
                case 0 -> wrap(n); case 1 -> fnApply(n); case 2 -> objEq(n); case 3 -> strValueOf(n);
                case 4 -> setIter(n); case 5 -> arrClone(n); case 6 -> appendOp(n); case 7 -> reqNonNull(n);
                case 8 -> idHash(n); case 9 -> listAdd(n); case 10 -> ipTest(n); case 12 -> supGet(n); case 13 -> biApply(n); case 14 -> uopApply(n); case 15 -> ofnApply(n); default -> ctl(n);
            };
            long dt = System.nanoTime() - t0; acc += v; if (dt < best) best = dt;
        }
        System.out.println(nm + " " + (best / n) + " ns/op");
        return acc;
    }

    public static void main(String[] a0) {
        for (int i = 0; i < 8; i++) { sarr[i] = "s" + i; set.add("k" + i); hm.put("k" + i, "v"); }
        int n = 100_000, reps = 8; long acc = 0;
        acc += time("DoubleArrayList.wrap ", 0, n, reps);
        acc += time("Function.apply       ", 1, n, reps);
        acc += time("Objects.equals       ", 2, n, reps);
        acc += time("String.valueOf(int)  ", 3, n, reps);
        acc += time("Set.iterator loop    ", 4, n / 8, reps);
        acc += time("Object[].clone       ", 5, n, reps);
        acc += time("StringBuilder.append ", 6, n, reps);
        acc += time("Objects.requireNonNul", 7, n, reps);
        acc += time("identityHashCode     ", 8, n, reps);
        acc += time("List.add             ", 9, n, reps);
        acc += time("IntPredicate.test    ", 10, n, reps);
        acc += time("Function.apply(Object)", 15, n, reps);
        acc += time("Supplier.get         ", 12, n, reps);
        acc += time("BiFunction.apply     ", 13, n, reps);
        acc += time("UnaryOperator.apply  ", 14, n, reps);
        acc += time("control              ", 11, n, reps);
        System.out.println("acc=" + acc);
    }
}
