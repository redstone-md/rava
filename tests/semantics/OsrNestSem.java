// An inner loop entered on stack replacement, whose header the outer loop's back edge also
// reaches: the inner loop gets a header of its own, fields it only reads move out of it, and
// they are read again when the outer loop changes them (directly, through a call, or through
// another reference to the same object).
public class OsrNestSem {
    static final class Q { float x = 1, y = 2, z = 3, w = 4; int n; }
    static final class Other { long a; int b; }

    static void bump(Q q, int k) { q.y += k; q.n++; }

    public static void main(String[] args) {
        Q q = new Q(), alias = q;
        Other o = new Other();
        double s = 0;
        long t = 0;
        for (int outer = 0; outer < 300; outer++) {
            for (int i = 0; i < 20000; i++) {
                float inv = 1f / (q.x * q.x + q.y * q.y + q.z * q.z + q.w * q.w);
                s += inv * i + q.n;
                o.a += i;
                o.b ^= i;
                if (i == 19999 && (outer & 7) == 3) alias.z += 1;
            }
            if ((outer & 1) == 0) q.x += 0.5f; else bump(q, outer);
            t += o.a + o.b;
        }
        System.out.println(s + " " + t + " " + q.x + " " + q.y + " " + q.z + " " + q.n);
    }
}
