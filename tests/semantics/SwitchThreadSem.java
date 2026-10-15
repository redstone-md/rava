// A switch on a value that some paths into it fix as constants (an inlined method returning
// codes that its caller switches on), including the default, cases sharing a target and a
// path whose value is not known.
public class SwitchThreadSem {
    static int code(int x) {
        if (x < 0) return -2;
        if (x == 0) return -1;
        if ((x & 3) == 1) return 7;
        if ((x & 3) == 2) return 100;
        return x;
    }
    static long run(int x) {
        switch (code(x)) {
            case -2: return 11;
            case -1: return 22;
            case 7: case 8: return 33;
            case 100: return 44;
            default: return x * 3L;
        }
    }
    public static void main(String[] args) {
        long h = 0;
        for (int i = -1000; i < 2_000_000; i++) h = h * 31 + run(i % 50 == 0 ? -i : i);
        System.out.println(h);
    }
}
