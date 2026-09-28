public class AotLiteralSem {
    static int initialized;
    static class Target { static { initialized++; } int n; Target(int n) { this.n = n; } }
    static String literal() { return "stable-literal"; }
    static Class<?> type() { return Target.class; }
    static Class<?> arrayType() { return String[].class; }
    static boolean keep() {
        String text = literal(); Class<?> cls = type(); int sum = 0;
        for (int i = 0; i < 20000; i++) { Target t = new Target(i); sum += t.n; }
        return text == literal() && cls == type() && sum == 199990000;
    }
    public static void main(String[] args) {
        System.out.println(literal().equals("stable-literal"));
        System.out.println(type() == Target.class);
        System.out.println(arrayType() == String[].class);
        System.out.println(initialized);
        System.out.println(keep());
        System.out.println(initialized);
    }
}
