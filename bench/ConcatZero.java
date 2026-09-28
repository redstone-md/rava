public class ConcatZero {
    static String name(int x, int z) {
        return "r." + x + "." + z + ".mca";
    }
    static long hot(int n) {
        long acc = 0;
        for (int i = 0; i < n; i++) {
            String s = name((i & 63) - 32, ((i >> 6) & 63) - 32);
            acc = acc * 31 + s.length();
        }
        return acc;
    }
    public static void main(String[] a) {
        for (int w = 0; w < 15; w++) hot(50_000);
        System.out.println("SUM=" + hot(200_000));
        System.out.println("N00=[" + name(0, 0) + "]");
        System.out.println("N0m1=[" + name(0, -1) + "]");
        System.out.println("Nm1m1=[" + name(-1, -1) + "]");
        System.out.println("N12m34=[" + name(12, -34) + "]");
    }
}
