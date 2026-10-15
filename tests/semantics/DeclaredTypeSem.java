// Casts and instanceof tests on values loaded from fields (their declared types: an array of
// objects, an interface, a class), null among them, and on any reference against Object.
public class DeclaredTypeSem {
    interface Shape { int area(); }
    static final class Sq implements Shape { final int s; Sq(int s) { this.s = s; } public int area() { return s * s; } }
    static class Holder { Object[] arr; Shape shape; CharSequence text; Object any; }

    static long test(Holder h) {
        long r = 0;
        if (h.arr instanceof Object[]) r += ((Object[]) h.arr).length;
        if (h.shape instanceof Shape) r += ((Shape) h.shape).area();
        if (h.text instanceof CharSequence) r += h.text.length();
        if (h.text instanceof String) r += 1000;
        Object a = h.any;
        if (a instanceof Object) r += 7;
        if (a != null && a instanceof Object[]) r += 11;
        Object x = (Object) h.shape;
        if (x instanceof Sq) r += 13;
        return r;
    }

    public static void main(String[] args) {
        Holder[] hs = new Holder[6];
        for (int i = 0; i < hs.length; i++) {
            Holder h = new Holder();
            if (i % 2 == 0) h.arr = i % 4 == 0 ? new String[3] : new Object[2];
            if (i % 3 != 0) h.shape = new Sq(i);
            h.text = i == 1 ? null : i % 2 == 0 ? "abc" : new StringBuilder("xy");
            h.any = i == 4 ? null : i == 5 ? new Integer[1] : "s";
            hs[i] = h;
        }
        long t = 0;
        for (int i = 0; i < 300000; i++) t = t * 31 + test(hs[i % hs.length]);
        System.out.println(t);
    }
}
