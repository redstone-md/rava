public class ObjectLeafSem {
    Object value;
    int number;
    static Object pressure;

    static Object exchange(ObjectLeafSem first, ObjectLeafSem second, Object value) {
        first.value = value;
        return second.value;
    }
    static Object call(ObjectLeafSem first, ObjectLeafSem second, Object value) {
        return exchange(first, second, value);
    }
    static int numbers(ObjectLeafSem first, ObjectLeafSem second, int value) {
        first.number = value;
        return second.number;
    }
    static int callNumbers(ObjectLeafSem first, ObjectLeafSem second, int value) {
        return numbers(first, second, value);
    }
    final Object instanceExchange(ObjectLeafSem second, Object value) {
        this.value = value;
        return second.value;
    }
    private Object privateExchange(Object value) {
        this.value = value;
        return this.value;
    }
    static Object callInstance(ObjectLeafSem first, ObjectLeafSem second, Object value) {
        return first.instanceExchange(second, value);
    }
    static Object callPrivate(ObjectLeafSem first, Object value) {
        return first.privateExchange(value);
    }
    public static void main(String[] args) {
        ObjectLeafSem a = new ObjectLeafSem(), b = new ObjectLeafSem();
        Object original = new Object(), replacement = new Object();
        b.value = original;
        b.number = 123;
        int matches = 0;
        for (int i = 0; i < 20000; i++) {
            if (call(a, b, replacement) == original && a.value == replacement) matches++;
            if (call(a, a, original) == original) matches++;
        }
        System.out.println("alias=" + matches);
        System.out.println("distinct-int=" + callNumbers(a, b, -456));
        System.out.println("alias-int=" + callNumbers(a, a, Integer.MIN_VALUE));
        Object retained = call(a, a, new Object());
        for (int i = 0; i < 128; i++) pressure = new byte[65536];
        System.out.println("retained=" + (retained == a.value));
        System.out.println("instance=" + (callInstance(a, b, replacement) == original));
        System.out.println("private=" + (callPrivate(a, replacement) == replacement));
        if (args.length != 0) return; // Separate non-null admission trace from fault fallback.
        try { call(a, null, replacement); System.out.println("missing-second-fault"); }
        catch (NullPointerException e) { System.out.println("second-null=" + (a.value == replacement)); }
        b.value = original;
        try { call(null, b, replacement); System.out.println("missing-first-fault"); }
        catch (NullPointerException e) { System.out.println("first-null=" + (b.value == original)); }
        System.out.println("null-value=" + (call(a, a, null) == null));
        try { callInstance(a, null, replacement); System.out.println("missing-instance-second"); }
        catch (NullPointerException e) { System.out.println("instance-second=" + (a.value == replacement)); }
        try { callInstance(null, b, replacement); System.out.println("missing-instance-first"); }
        catch (NullPointerException e) { System.out.println("instance-first=" + (b.value == original)); }
        try { callPrivate(null, replacement); System.out.println("missing-private-null"); }
        catch (NullPointerException e) { System.out.println("private-null"); }
    }
}
