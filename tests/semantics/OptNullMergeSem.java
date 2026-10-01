/** Null checks across merges and loops: a value checked on one path only, loops entered with a
 *  checked value, and loops whose body can make a slot null. */
public class OptNullMergeSem {
    static final class Box { int v; Box next; Box(int v, Box n) { this.v = v; next = n; } }

    static int oneSided(Box b, boolean check) {
        int s = 0;
        if (check) s += b.v;        // checked on this path only
        try {
            s += b.v * 2;           // must still throw when b is null and !check
        } catch (NullPointerException e) {
            s -= 1000;
        }
        return s;
    }

    static int loop(Box b, int n) {
        int s = b.v;                // checked before the loop
        for (int i = 0; i < n; i++) s += b.v + i;
        return s;
    }

    static int walk(Box b) {
        int s = 0;
        try {
            while (true) { s += b.v; b = b.next; }   // b becomes null inside the loop
        } catch (NullPointerException e) {
            return s;
        }
    }

    static int doWhile(Box b, int k) {
        int s = 0;
        do {
            s += b.v;
            if (k-- == 2) b = null;
        } while (k > 0 && b != null);
        return s;
    }

    public static void main(String[] args) {
        Box list = new Box(1, new Box(2, new Box(3, null)));
        long h = 0;
        for (int i = 0; i < 200000; i++) {
            Box b = (i % 3 == 0) ? null : list;
            h = h * 31 + oneSided(b, b != null && (i & 1) == 0);
            if (b != null) h = h * 31 + loop(b, i & 7);
            h = h * 31 + walk(list);
            h = h * 31 + doWhile(list, i & 5);
        }
        System.out.println(h);
    }
}
