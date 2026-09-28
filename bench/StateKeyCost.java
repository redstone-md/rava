// WHAT DOES A MAP KEY WHOSE EQUALITY IS IDENTITY COST? — `RAVA_KEYTIME` prices a boot's
// structural key renders at 912,225 x 5,004 ns, and `RAVA_STRUCTKEY_SCAN` names the class:
// `BlockState`, 32 fields. Its effective `equals` is `StateHolder`'s, whose body is
// `return super.equals(o)` into `java/lang/Object` — reference identity, stated in bytecode.
//
// This reproduces exactly that shape so the low-noise instrument can price the rule the boot
// cannot: `Holder` declares `equals` and delegates to Object's, so it is admissible; `ValueKey`
// declares a real field-comparing `equals`, so it must NOT be, and its timing is the control
// that proves the rule discriminates instead of admitting everything.
//
// ⚠Each loop lives in its own method called twice, so it reaches the compiled tier by entry
// count — a loop in `main` measures the interpreter wearing a product label.
import java.util.*;

public class StateKeyCost {
    /** `StateHolder`'s shape: a declared `equals` that delegates straight to `Object`. */
    static class Holder {
        final int a, b, c, d;
        Holder(int a, int b, int c, int d) { this.a = a; this.b = b; this.c = c; this.d = d; }
        @Override public boolean equals(Object o) { return super.equals(o); }
        @Override public int hashCode() { return super.hashCode(); }
        @Override public String toString() { return "Holder[" + a + "," + b + "," + c + "," + d + "]"; }
    }

    /** The CONTROL: real value equality. The rule must refuse this one. */
    static class ValueKey {
        final int a, b, c, d;
        ValueKey(int a, int b, int c, int d) { this.a = a; this.b = b; this.c = c; this.d = d; }
        @Override public boolean equals(Object o) {
            if (!(o instanceof ValueKey)) return false;
            ValueKey v = (ValueKey) o;
            return a == v.a && b == v.b && c == v.c && d == v.d;
        }
        @Override public int hashCode() { return ((a * 31 + b) * 31 + c) * 31 + d; }
        @Override public String toString() { return "ValueKey[" + a + "," + b + "," + c + "," + d + "]"; }
    }

    static final int N = 512;
    static Holder[] hk = new Holder[N];
    static ValueKey[] vk = new ValueKey[N];
    static HashMap<Object, Object> hm = new HashMap<>();
    static HashMap<Object, Object> vm = new HashMap<>();

    static long getHolder(int n) { long a = 0; for (int i = 0; i < n; i++) a += hm.get(hk[i & (N - 1)]) == null ? 0 : 1; return a; }
    static long getValue(int n)  { long a = 0; for (int i = 0; i < n; i++) a += vm.get(vk[i & (N - 1)]) == null ? 0 : 1; return a; }
    static long control(int n)   { long a = 0; for (int i = 0; i < n; i++) a += (hk[i & (N - 1)] == null ? 0 : 1); return a; }

    static long time(String name, int which, int n, int reps) {
        long best = Long.MAX_VALUE, acc = 0;
        for (int r = 0; r < reps; r++) {
            long t0 = System.nanoTime();
            long v = which == 0 ? getHolder(n) : which == 1 ? getValue(n) : control(n);
            long dt = System.nanoTime() - t0;
            acc += v;
            if (dt < best) best = dt;
        }
        System.out.println(name + " " + (best / n) + " ns/call");
        return acc;
    }

    public static void main(String[] args) {
        for (int i = 0; i < N; i++) {
            hk[i] = new Holder(i, i * 2, i * 3, i * 5);
            vk[i] = new ValueKey(i, i * 2, i * 3, i * 5);
            hm.put(hk[i], hk[i]);
            vm.put(vk[i], vk[i]);
        }
        int n = 200_000, reps = 6;
        long acc = 0;
        acc += time("identity-equals key ", 0, n, reps);
        acc += time("value-equals key    ", 1, n, reps);
        acc += time("control loop        ", 2, n, reps);
        System.out.println("acc=" + acc);
    }
}
