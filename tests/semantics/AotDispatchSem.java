public class AotDispatchSem {
    public interface Op { long step(int x, long y); }
    public static class Base implements Op {
        int count;
        public Base(int count) { this.count = count; }
        public long step(int x, long y) {
            for (int i = 0; i < 1; i++) {}
            return y + x + count;
        }
        public synchronized int locked() { return ++count; }
        public Base retain(int count) {
            for (int i = 0; i < count; i++) {
                Base temporary = new Base(i);
                if (temporary.count < 0) return temporary;
            }
            return this;
        }
    }
    public static class Derived extends Base {
        public Derived(int count) { super(count); }
        public long step(int x, long y) {
            for (int i = 0; i < 1; i++) {}
            return y - x - count;
        }
    }
    public static class Inherited extends Base {
        public Inherited(int count) { super(count); }
    }
    public static class EpochBump { int value = 42; }
    public static class Init {
        static { System.out.println("init"); }
        public static long number(long value) {
            for (int i = 0; i < 1; i++) {}
            return value + 7;
        }
    }
    static long call(Op receiver, int x, long y) {
        for (int i = 0; i < 1; i++) {}
        return receiver.step(x, y);
    }
    static long callInit(long value) {
        for (int i = 0; i < 1; i++) {}
        return Init.number(value);
    }
    static Base keep(Base object, int count) {
        for (int i = 0; i < 1; i++) {}
        return object.retain(count);
    }
    static int lock(Base object) {
        for (int i = 0; i < 1; i++) {}
        return object.locked();
    }
    public static void main(String[] args) {
        System.out.println("before");
        System.out.println(callInit(3));
        System.out.println(callInit(4));
        Base first = new Base(11), second = new Derived(17);
        Op[] receivers = {first, second, new Inherited(23)};
        long total = 0;
        for (int i = 0; i < 3000; i++) {
            if (i == 1500 && new EpochBump().value != 42) throw new AssertionError();
            total += call(receivers[i % 3], i, 10000000000L);
        }
        System.out.println(total);
        System.out.println(lock(first));
        try { call(null, 1, 2); System.out.println("missing NPE"); }
        catch (NullPointerException expected) { System.out.println("NPE"); }
        System.out.println(keep(first, 10000) == first);
        System.out.println(first.count);
    }
}
