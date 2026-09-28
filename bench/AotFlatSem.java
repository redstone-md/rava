public class AotFlatSem {
    public static class Box {
        int value = 42;
        public int get() { return value; }
        public static Object identity(Object value) { return value; }
    }
    static int call(Box box) {
        for (int i = 0; i < 1; i++) {}
        return box.get();
    }
    static Object identity(Object value) {
        for (int i = 0; i < 1; i++) {}
        return Box.identity(value);
    }
    public static void main(String[] args) {
        Box box = new Box();
        int sum = 0;
        for (int i = 0; i < 20000; i++) sum += call(box);
        System.out.println(sum);
        System.out.println(identity(box) == box);
        System.out.println(identity(null) == null);
        try { call(null); System.out.println("missing NPE"); }
        catch (NullPointerException expected) { System.out.println("NPE"); }
    }
}
