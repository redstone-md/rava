/** Megamorphic calls with short-lived computed register arguments. Compare alternating
 *  process runs on the same host; each process warms both loops before reporting ns/call. */
public class DispatchArgBench {
    interface Op { long eval(int a, long b, float f, double d); }
    static abstract class Base implements Op {
        int a = -123456789;
        long b = 0x123456789abcdefL;
        float f = -0.75f;
        double d = 1.25;
        public abstract long eval(int a, long b, float f, double d);
    }
    static final class V0 extends Base {
        public long eval(int a, long b, float f, double d) { return b + a + (long) (f + d); }
    }
    static final class V1 extends Base {
        public long eval(int a, long b, float f, double d) { return b ^ a ^ (long) (f - d); }
    }
    static final class V2 extends Base {
        public long eval(int a, long b, float f, double d) { return b - a + (long) (f * d); }
    }
    static final class V3 extends Base {
        public long eval(int a, long b, float f, double d) { return (b | a) - (long) (f + d); }
    }

    static long virtualLoop(Base[] ops, int count) {
        long sum = 0;
        for (int i = 0; i < count; i++) {
            sum += ops[i & 3].eval(i * 31 - 7, ((long) i << 33) + 11, i * 0.25f, i * -0.125);
        }
        return sum;
    }

    static long interfaceLoop(Op[] ops, int count) {
        long sum = 0;
        for (int i = 0; i < count; i++) {
            sum += ops[i & 3].eval(i * 31 - 7, ((long) i << 33) + 11, i * 0.25f, i * -0.125);
        }
        return sum;
    }

    static long virtualFields(Base[] ops, int count) {
        long sum = 0;
        for (int i = 0; i < count; i++) {
            Base receiver = ops[i & 3];
            sum += receiver.eval(receiver.a, receiver.b, receiver.f, receiver.d);
        }
        return sum;
    }

    static long interfaceFields(Base[] ops, int count) {
        long sum = 0;
        for (int i = 0; i < count; i++) {
            Base fields = ops[i & 3];
            Op receiver = fields;
            sum += receiver.eval(fields.a, fields.b, fields.f, fields.d);
        }
        return sum;
    }

    public static void main(String[] args) {
        int count = args.length > 0 ? Integer.parseInt(args[0]) : 1_000_000;
        int rounds = args.length > 1 ? Integer.parseInt(args[1]) : 7;
        Base[] virtual = {new V0(), new V1(), new V2(), new V3()};
        Op[] inter = virtual;
        long check = 0;
        for (int round = 0; round < 12; round++) {
            check ^= virtualLoop(virtual, count);
            check += interfaceLoop(inter, count);
            check ^= virtualFields(virtual, count);
            check += interfaceFields(virtual, count);
        }
        for (int round = 0; round < rounds; round++) {
            long t0 = System.nanoTime();
            long v = virtualLoop(virtual, count);
            long t1 = System.nanoTime();
            long f = interfaceLoop(inter, count);
            long t2 = System.nanoTime();
            long vf = virtualFields(virtual, count);
            long t3 = System.nanoTime();
            long ff = interfaceFields(virtual, count);
            long t4 = System.nanoTime();
            check ^= v + f + vf + ff;
            System.out.printf("virtual %.2f ns/call interface %.2f ns/call%n", (t1 - t0) / (double) count, (t2 - t1) / (double) count);
            System.out.printf("virtual fields %.2f ns/call interface fields %.2f ns/call%n", (t3 - t2) / (double) count, (t4 - t3) / (double) count);
        }
        System.out.println("check=" + check);
    }
}
