// A compiled set/get must use the same storage as the native remove.
public class ThreadLocalRemoveSem {
    static final ThreadLocal<Object> value = new ThreadLocal<>();
    static Object cycle(Object token) {
        value.set(token);
        if (value.get() != token) throw new AssertionError("set/get");
        value.remove();
        return value.get();
    }
    public static void main(String[] args) {
        Object token = new Object();
        int stale = 0;
        for (int i = 0; i < 10000; i++) if (cycle(token) != null) stale++;
        System.out.println("stale=" + stale);
    }
}
