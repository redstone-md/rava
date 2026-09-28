// Same invocation site sees computed and trivial getters in different orders.
public class GetterOrderSem {
    abstract static class Base { abstract int get(); }
    static final class Computed extends Base {
        int value;
        Computed(int value) { this.value = value; }
        int get() { return value + 1; }
    }
    static final class Plain extends Base {
        int value;
        Plain(int value) { this.value = value; }
        int get() { return value; }
    }
    static int read(Base value) { return value.get(); }
    public static void main(String[] args) {
        Computed computed = new Computed(6);
        Plain plain = new Plain(19);
        Base[] values = {computed, plain, plain, computed};
        for (int phase = 0; phase < 4; phase++) {
            int sum = 0;
            for (int i = 0; i < 2000; i++) sum += read(values[(i + phase) & 3]);
            System.out.println("phase=" + phase + " sum=" + sum);
            computed.value += 3;
            plain.value -= 2;
        }
    }
}
