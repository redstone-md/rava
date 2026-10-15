// A field stored with the value it already holds: dropped only when nothing in between (a call,
// a store through another reference that may be the same object, a store on another path) may
// have changed it.
public class StoreSameSem {
    static final class P { int x; float f; Object o; }
    static int calls;

    static void poke(P p) { calls++; p.x = 77; p.f = 7.5f; }

    static int aliased(P a, P b, int v) {
        a.x = v;
        b.x = 5;
        a.x = v;
        return a.x + b.x;
    }

    static int called(P a, int v) {
        a.x = v;
        if ((v & 7) == 0) poke(a);
        a.x = v;
        return a.x;
    }

    static float copied(P a, P b) {
        float f = a.f;
        b.f = f * 2;
        a.f = f;
        return a.f + b.f;
    }

    static int branches(P a, int v) {
        int x = a.x;
        if ((v & 1) == 0) a.x = v;
        a.x = x;
        return a.x;
    }

    public static void main(String[] args) {
        P a = new P(), b = new P();
        long s = 0;
        for (int i = 0; i < 300000; i++) {
            s += aliased(a, (i & 3) == 0 ? a : b, i);
            s += called(a, i);
            a.f = i;
            s += (long) copied(a, (i & 1) == 0 ? a : b);
            s += branches(a, i);
        }
        System.out.println(s + " " + calls + " " + a.x + " " + b.x + " " + a.f + " " + b.f);
    }
}
