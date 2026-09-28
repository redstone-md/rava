public class AotDirectSem {
    int value;
    static long wide(int a, long b, double c, float d) {
        for (int i = 0; i < 1; i++) {}
        return a + b + (long)c + (long)d;
    }
    static long callWide(int a, long b, double c, float d) {
        for (int i = 0; i < 1; i++) {}
        return wide(a, b, c, d);
    }
    static Object select(Object a, Object b, boolean first) {
        for (int i = 0; i < 1; i++) {}
        return first ? a : b;
    }
    static Object callSelect(Object a, Object b, boolean first) {
        for (int i = 0; i < 1; i++) {}
        return select(a, b, first);
    }
    static int mutate(AotDirectSem object, int value) {
        for (int i = 0; i < 1; i++) {}
        object.value += value;
        return object.value;
    }
    static int callMutate(AotDirectSem object, int value) {
        for (int i = 0; i < 1; i++) {}
        return mutate(object, value);
    }
    static AotDirectSem allocate(int value) {
        for (int i = 0; i < 1; i++) {}
        AotDirectSem object = new AotDirectSem();
        object.value = value;
        return object;
    }
    static AotDirectSem callAllocate(int value) {
        for (int i = 0; i < 1; i++) {}
        return allocate(value);
    }
    static AotDirectSem retain(AotDirectSem keep, int count) {
        for (int i = 0; i < count; i++) {
            AotDirectSem temporary = allocate(i);
            if (temporary.value < 0) return temporary;
        }
        return keep;
    }
    static AotDirectSem callRetain(AotDirectSem keep, int count) {
        for (int i = 0; i < 1; i++) {}
        return retain(keep, count);
    }
    public static void main(String[] args) {
        System.out.println(callWide(-7, Long.MIN_VALUE, -2.5, 4.75f));
        Object a = new Object(), b = new int[3];
        System.out.println(callSelect(a, b, true) == a);
        System.out.println(callSelect(a, b, false) == b);
        System.out.println(callSelect(null, b, true) == null);
        System.out.println(callSelect(Boolean.TRUE, Boolean.FALSE, true) == Boolean.TRUE);
        AotDirectSem object = callAllocate(40);
        System.out.println(callMutate(object, 2));
        try { callMutate(null, 1); System.out.println("missing NPE"); }
        catch (NullPointerException expected) { System.out.println("NPE"); }
        long total = 0;
        for (int i = 0; i < 10000; i++) total += callAllocate(i).value;
        System.out.println(total);
        System.out.println(callRetain(object, 10000) == object);
        System.out.println(object.value);
    }
}
