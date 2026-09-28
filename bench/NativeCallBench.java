// ONE natively-served call per iteration, and nothing else.
//
// The population this measures is the one named in-tree as "EVENT invokevirtual -> NATIVE branch
// 17,337,632" per boot: a guest call whose receiver class the native layer serves, so it leaves
// the interpreter through `natives::try_invoke` and enters `dispatch_owner_cascade`. CtorBench
// stopped covering that path the moment `Object.<init>` was short-circuited, and no other bench
// in the tree runs interpreted for long enough to measure it.
import java.util.ArrayList;

public class NativeCallBench {
    static long loop(int n, ArrayList<Integer> l) {
        long t0 = System.nanoTime();
        int s = 0;
        for (int i = 0; i < n; i++) s += l.get(0);
        long t1 = System.nanoTime();
        if (s == -1) System.out.println("never");
        return t1 - t0;
    }

    public static void main(String[] args) {
        int n = args.length > 0 ? Integer.parseInt(args[0]) : 2000000;
        ArrayList<Integer> l = new ArrayList<>();
        l.add(7);
        for (int r = 1; r <= 4; r++) {
            System.out.println("RUN" + r + "_NS=" + (loop(n, l) / (double) n));
        }
    }
}
