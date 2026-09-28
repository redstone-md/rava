// JVM-parity probe for record hashCode/equals semantics (RAVA_RECORD_SEMANTICS).
// Record hashes are DETERMINISTIC on the JVM (unlike identity hashes), so every
// line printed here must be byte-identical between `java RecordSemProbe` and
// `rava -cp . RecordSemProbe` with the flag on. Covers: every primitive kind
// (incl. boolean 1231/1237, float/double NaN canonicalisation and signed-zero
// bit rules), String, null, boxed, and nested-record recursion.
public class RecordSemProbe {
    record P(int i, long l, boolean z, char c, byte b, short s) {}
    record F(float f, double d) {}
    record S(String name, String path) {}
    record N(S key, P prim, Integer box, String nil) {}
    record Deep(N a, N b, F f) {}

    static void line(String k, Object v) { System.out.println(k + "=" + v); }

    public static void main(String[] args) {
        P p1 = new P(42, 0x1234_5678_9abc_def0L, true, 'x', (byte) -7, (short) 300);
        P p2 = new P(42, 0x1234_5678_9abc_def0L, true, 'x', (byte) -7, (short) 300);
        P p3 = new P(42, 0x1234_5678_9abc_def0L, false, 'x', (byte) -7, (short) 300);
        line("P1_HASH", p1.hashCode());
        line("P3_HASH", p3.hashCode());
        line("P_EQ", p1.equals(p2));
        line("P_NE", p1.equals(p3));

        F fNan1 = new F(Float.NaN, Double.NaN);
        F fNan2 = new F(Float.intBitsToFloat(0x7fc00001), Double.NaN); // non-canonical NaN
        F fZero = new F(0.0f, 0.0d);
        F fNegZero = new F(-0.0f, -0.0d);
        line("F_NAN_HASH", fNan1.hashCode());
        line("F_NAN2_HASH", fNan2.hashCode());
        line("F_NAN_EQ", fNan1.equals(fNan2)); // true: Float/Double.compare semantics
        line("F_ZERO_HASH", fZero.hashCode());
        line("F_NEGZERO_HASH", fNegZero.hashCode());
        line("F_ZERO_NE", fZero.equals(fNegZero)); // false: signed zeros differ
        line("F_PLAIN_HASH", new F(1.5f, -2.25d).hashCode());

        S s1 = new S("minecraft", "overworld");
        S s2 = new S("minecraft", "overworld");
        line("S_HASH", s1.hashCode());
        line("S_EQ", s1.equals(s2));
        line("S_NE", s1.equals(new S("minecraft", "the_nether")));

        N n1 = new N(s1, p1, 12345, null);
        N n2 = new N(s2, p2, 12345, null);
        N n3 = new N(s2, p2, 12345, "set");
        line("N_HASH", n1.hashCode());
        line("N3_HASH", n3.hashCode());
        line("N_EQ", n1.equals(n2)); // deep recursion through S and P
        line("N_NE", n1.equals(n3)); // null vs non-null component
        line("N_SELF_EQ", n1.equals(n1));
        line("N_OTHER_TYPE", n1.equals("not a record"));

        Deep d1 = new Deep(n1, n2, fZero);
        Deep d2 = new Deep(n2, n1, fZero);
        line("DEEP_HASH", d1.hashCode());
        line("DEEP_EQ", d1.equals(d2));
    }
}
