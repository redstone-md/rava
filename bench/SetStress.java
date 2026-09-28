// Exercises the set-membership-index self-healing invariant: every mutation
// that bypasses the fast-path add (remove, addAll, retainAll, clear,
// iterator.remove) changes the backing length, which must force the index to
// rebuild so a later contains()/add() never reads a stale index. Prints a
// checksum the JVM and rava must agree on bit-for-bit.
import java.util.*;
public class SetStress {
    public static void main(String[] a) {
        long acc = 0;
        Set<Integer> s = new HashSet<>();
        // build
        for (int i = 0; i < 200; i++) s.add(i);
        acc += s.size();                                 // 200
        // remove-then-add same value (net-neutral length across the pair)
        s.remove(50); s.add(50);
        acc += s.contains(50) ? 1 : 0;                   // 1
        acc += s.contains(999) ? 1000 : 0;               // 0
        // remove a span, then re-probe membership
        for (int i = 0; i < 100; i++) s.remove(i);
        acc += s.size();                                 // 100
        acc += s.contains(40) ? 5 : 0;                   // 0 (removed)
        acc += s.contains(150) ? 7 : 0;                  // 7 (present)
        // addAll overlap (dedup) then re-add via fast path
        List<Integer> more = new ArrayList<>();
        for (int i = 90; i < 260; i++) more.add(i);
        s.addAll(more);
        acc += s.size();                                 // {100..259} = 160
        s.add(300); s.add(300);                          // fast-path dedup after addAll
        acc += s.size();                                 // 161
        // iterator.remove drops evens, contains must see it
        for (Iterator<Integer> it = s.iterator(); it.hasNext();) {
            if ((it.next() & 1) == 0) it.remove();
        }
        acc += s.contains(200) ? 9 : 0;                  // 0 (even, removed)
        acc += s.contains(201) ? 11 : 0;                 // 11 (odd, kept)
        // clear then rebuild
        int before = s.size();
        s.clear();
        s.add(7); s.add(7); s.add(8);
        acc += before * 100000L + s.size();              // before*1e5 + 2
        // retainAll
        Set<Integer> keep = new HashSet<>(Arrays.asList(7));
        s.retainAll(keep);
        acc += s.contains(7) ? 13 : 0;                   // 13
        acc += s.contains(8) ? 17 : 0;                   // 0
        System.out.println("SUM=" + acc);
    }
}
