// Unboxing a value boxed in the same compiled code reads the value (C2's EliminateAutoBox),
// while boxes still compare by identity where the code asks (cached ones are shared, others
// are not), and a supplier that returns null still throws on unboxing.
import java.util.function.Supplier;

public class BoxElimSem {
    static long mix(int i) {
        Integer a = i, b = i;
        Long l = (long) i * 3;
        Character c = (char) ('a' + (i & 15));
        Short s = (short) (i - 500);
        Byte y = (byte) i;
        return a + (a == b ? 1000 : 0) + l + c + s + y;
    }

    static long sum(Supplier<Integer>[] fs, int n) {
        long t = 0;
        for (int r = 0; r < n; r++) for (Supplier<Integer> f : fs) t += f.get();
        return t;
    }

    @SuppressWarnings("unchecked")
    public static void main(String[] args) {
        long acc = 0;
        for (int r = 0; r < 40000; r++) acc += mix(r % 400 - 200);
        System.out.println("mix " + acc);
        Supplier<Integer>[] fs = new Supplier[20];
        for (int i = 0; i < fs.length; i++) { int j = i * 13 - 100; fs[i] = i % 2 == 0 ? () -> j : () -> j * 2; }
        long t = 0;
        for (int r = 0; r < 300; r++) t += sum(fs, 50);
        System.out.println("sum " + t);
        fs[3] = () -> null;
        try {
            sum(fs, 1);
        } catch (NullPointerException e) {
            System.out.println("npe");
        }
        System.out.println(mix(5) + " " + mix(300) + " " + mix(-129));
    }
}
