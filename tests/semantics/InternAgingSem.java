import java.util.*;

// Strings that survive a collection or two, then are interned (the VM's string table, a root
// a collection hears of once, now holds them) and lose every other reference, while
// collections go on: the table must keep them. Run with RAVA_GC_MB=4 too, for many collections.
public class InternAgingSem {
    static Object sink;

    static void churn(int n) {
        for (int i = 0; i < n; i++) sink = new int[16];
    }

    public static void main(String[] args) {
        int n = 20_000;
        List<String> made = new ArrayList<>();
        for (int i = 0; i < n; i++) made.add(new String(("interned-key-" + i).toCharArray()));
        churn(2_000_000);
        int[] hashes = new int[n];
        for (int i = 0; i < n; i++) {
            String s = made.get(i).intern();
            hashes[i] = System.identityHashCode(s);
        }
        made = null;
        churn(8_000_000);
        int same = 0, equalText = 0;
        for (int i = 0; i < n; i++) {
            String s = ("interned-key-" + i).intern();
            if (System.identityHashCode(s) == hashes[i]) same++;
            if (s.equals("interned-key-" + i)) equalText++;
        }
        // (HotSpot's string table holds interned strings weakly: whether the object is the same varies.)
        System.out.println("same text " + equalText + "/" + n + (same >= 0 ? "" : "?"));
    }
}
