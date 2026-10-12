/** Branches whose two targets are one block (`if (c) {}` compiles to a jump to the next
 *  instruction), with paths a profile prunes merging after them, compiled and run hot. */
public class EmptyIfSem {
    boolean t = true;
    boolean on = true;
    int p;
    int a, b, c;

    void i() { a++; }
    void j() { b++; }
    void k() { c++; }

    void upload() {
        if (t) { }
        if (!on) { }
        t = false;
        if (p <= 3) i();
        else if (p <= 7) j();
        else if (p <= 10) k();
        else { System.out.println("bad " + p); return; }
    }

    public static void main(String[] args) {
        EmptyIfSem s = new EmptyIfSem();
        for (int r = 0; r < 200000; r++) {
            s.p = (r & 1) == 0 ? 9 : 2;
            s.upload();
        }
        s.p = 2; s.upload();
        s.p = 5; s.upload();
        s.p = 11; s.upload();
        System.out.println(s.a + " " + s.b + " " + s.c);
    }
}
