/** Register arguments of megamorphic virtual and interface calls: mixed widths, duplicate
 *  arguments, values live across calls, a new receiver, collection and exceptional exits. */
public class RegDispatchHintSem {
    static final class Token {
        final long id;
        Token(long id) { this.id = id; }
    }

    static final class Inputs {
        int a = -123456789;
        long b = 0x123456789abcdefL;
        Token token;
        float f = -0.75f;
        double d = 1.25;
        Inputs(Token token) { this.token = token; }
    }

    interface Op { long mix(int a, long b, Token token, float f, double d, int duplicate); }
    static abstract class Base implements Op {
        public abstract long mix(int a, long b, Token token, float f, double d, int duplicate);
    }
    static final class V0 extends Base {
        public long mix(int a, long b, Token t, float f, double d, int same) { return fold(a, b, t, f, d, same) + 1; }
    }
    static final class V1 extends Base {
        public long mix(int a, long b, Token t, float f, double d, int same) { return fold(a, b, t, f, d, same) + 3; }
    }
    static final class V2 extends Base {
        public long mix(int a, long b, Token t, float f, double d, int same) { return fold(a, b, t, f, d, same) + 5; }
    }
    static final class V3 extends Base {
        public long mix(int a, long b, Token t, float f, double d, int same) { return fold(a, b, t, f, d, same) + 7; }
    }
    static final class V4 extends Base {
        public long mix(int a, long b, Token t, float f, double d, int same) { return fold(a, b, t, f, d, same) + 9; }
    }

    static long fold(int a, long b, Token token, float f, double d, int duplicate) {
        if (a != duplicate) throw new AssertionError("duplicate argument changed");
        if (a == Integer.MIN_VALUE) throw new IllegalArgumentException("rare");
        if (a == Integer.MIN_VALUE + 1) System.gc();
        return b ^ (long) a ^ token.id ^ Double.doubleToLongBits(f + d);
    }

    static long virtualLoop(Base[] ops, Token token, int start, int count) {
        long sum = 0, live = token.id;
        for (int i = start; i < start + count; i++) {
            int a = i * 31 - 40000;
            sum += ops[i & 3].mix(a, ((long) i << 35) ^ live, token, i * 0.25f, i * -0.125, a);
            live ^= sum;
        }
        return sum ^ live ^ token.id;
    }

    static long interfaceLoop(Op[] ops, Token token, int start, int count) {
        long sum = 0, live = token.id;
        for (int i = start; i < start + count; i++) {
            int a = i * 31 - 40000;
            sum += ops[i & 3].mix(a, ((long) i << 35) ^ live, token, i * 0.25f, i * -0.125, a);
            live ^= sum;
        }
        return sum ^ live ^ token.id;
    }

    static long fieldLoop(Base[] virtual, Op[] inter, Inputs input, int count) {
        long sum = 0;
        for (int i = 0; i < count; i++) {
            sum ^= virtual[i & 3].mix(input.a, input.b, input.token, input.f, input.d, input.a);
            sum += inter[i & 3].mix(input.a, input.b, input.token, input.f, input.d, input.a);
        }
        return sum;
    }

    public static void main(String[] args) {
        Base[] virtual = {new V0(), new V1(), new V2(), new V3()};
        Op[] inter = virtual;
        Token token = new Token(0x1abcdef0091L);
        Inputs input = new Inputs(token);
        long v = 0, f = 0, fields = 0;
        for (int round = 0; round < 2000; round++) {
            v += virtualLoop(virtual, token, round * 64, 64);
            f += interfaceLoop(inter, token, round * 64, 64);
            fields += fieldLoop(virtual, inter, input, 64);
        }
        System.out.println("virtual=" + v);
        System.out.println("interface=" + f);
        System.out.println("fields=" + fields);
        virtual[0] = new V4();
        System.out.println("new=" + virtualLoop(virtual, token, 0, 64) + ":" + interfaceLoop(inter, token, 0, 64));
        System.out.println("gc=" + virtual[0].mix(Integer.MIN_VALUE + 1, -0x123456789L, token, -0.5f, 1.25, Integer.MIN_VALUE + 1));
        try { virtual[1].mix(Integer.MIN_VALUE, 0, token, 0, 0, Integer.MIN_VALUE); }
        catch (IllegalArgumentException e) { System.out.println("virtual exception"); }
        try { inter[2].mix(Integer.MIN_VALUE, 0, token, 0, 0, Integer.MIN_VALUE); }
        catch (IllegalArgumentException e) { System.out.println("interface exception"); }
    }
}
