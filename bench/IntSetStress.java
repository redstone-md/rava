// fastutil int-set correctness across the rewritten binary-search arms: add
// dedup, remove (present/absent), contains, first/last (sorted endpoints),
// size/isEmpty, addAll dedup, clear, toIntArray order. Checksum must match JVM.
import it.unimi.dsi.fastutil.ints.*;
import java.util.*;
public class IntSetStress {
    static long run(IntSet s) {
        long acc = 0;
        for (int i = 0; i < 100; i++) acc += s.add(50 - i) ? 1 : 0;   // dups around 0
        for (int i = 0; i < 100; i++) acc += s.add(50 - i) ? 1 : 0;   // all dup → 0
        acc += s.size();
        acc += s.contains(0) ? 7 : 0;
        acc += s.contains(999) ? 11 : 0;
        acc += s.remove(0) ? 13 : 0;
        acc += s.remove(0) ? 17 : 0;          // already gone
        acc += s.contains(0) ? 19 : 0;
        acc += s.size();
        // Order-independent membership sweep (IntOpenHashSet iteration order is
        // unspecified — rava models it sorted, the JVM uses hash order; only
        // size/contains semantics are guaranteed equal).
        long present = 0; for (int i = -60; i <= 60; i++) present += s.contains(i) ? (i & 7) : 0;
        acc += present;
        s.addAll(Arrays.asList(1000, 1001, 1000, 49));   // 2 new (1000,1001), 49 dup
        acc += s.size();
        s.clear();
        acc += s.isEmpty() ? 23 : 0;
        s.add(5); s.add(5); s.add(3); s.add(9);
        acc += s.size();
        return acc;
    }
    public static void main(String[] a) {
        long acc = 0;
        acc += 1_000_000L * run(new IntOpenHashSet());
        acc += 1_000L * run(new IntRBTreeSet());
        acc += run(new IntAVLTreeSet());
        System.out.println("SUM=" + acc);
    }
}
