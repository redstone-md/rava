public class NumMisc {
    static void p(String k, Object v) { System.out.println(k + "=" + v); }

    public static void main(String[] args) {
        // --- hex printing ----------------------------------------------------
        p("hx_1", Double.toHexString(1.0));
        p("hx_mz", Double.toHexString(-0.0));
        p("hx_z", Double.toHexString(0.0));
        p("hx_min", Double.toHexString(Double.MIN_VALUE));
        p("hx_minnorm", Double.toHexString(Double.MIN_NORMAL));
        p("hx_max", Double.toHexString(Double.MAX_VALUE));
        p("hx_p1", Double.toHexString(0.1));
        p("hx_m2", Double.toHexString(-2.0));
        p("hx_nan", Double.toHexString(Double.NaN));
        p("hx_inf", Double.toHexString(Double.POSITIVE_INFINITY));
        p("hxf_1", Float.toHexString(1.0f));
        p("hxf_p1", Float.toHexString(0.1f));
        p("hxf_min", Float.toHexString(Float.MIN_VALUE));
        p("hxf_minnorm", Float.toHexString(Float.MIN_NORMAL));
        p("hxf_max", Float.toHexString(Float.MAX_VALUE));
        p("hxf_mz", Float.toHexString(-0.0f));

        // --- boxed toString / compareTo / hashCode ----------------------------
        p("D_toString", Double.toString(1.0E-5));
        p("F_toString", Float.toString(0.1f));
        p("D_obj_toString", Double.valueOf(-0.0).toString());
        p("F_obj_toString", Float.valueOf(1e7f).toString());
        p("D_compareTo", Double.valueOf(0.0).compareTo(Double.valueOf(-0.0)));
        p("D_compareTo_nan", Double.valueOf(Double.NaN).compareTo(Double.valueOf(1.0)));
        p("F_compareTo", Float.valueOf(-0.0f).compareTo(Float.valueOf(0.0f)));
        p("I_compareTo", Integer.valueOf(Integer.MIN_VALUE).compareTo(Integer.valueOf(1)));
        p("L_compareTo", Long.valueOf(Long.MIN_VALUE).compareTo(Long.valueOf(1L)));
        p("D_hashCode_1", Double.hashCode(1.0));
        p("D_hashCode_mz", Double.hashCode(-0.0));
        p("D_hashCode_nan", Double.hashCode(Double.NaN));
        p("F_hashCode", Float.hashCode(1.5f));
        p("L_hashCode", Long.hashCode(-1L));
        p("I_hashCode", Integer.hashCode(-1));
        p("B_hashCode_t", Boolean.hashCode(true));
        p("B_hashCode_f", Boolean.hashCode(false));
        p("C_hashCode", Character.hashCode('A'));
        p("S_hashCode", Short.hashCode((short) -1));
        p("D_equals_mz", Double.valueOf(0.0).equals(Double.valueOf(-0.0)));
        p("D_equals_nan", Double.valueOf(Double.NaN).equals(Double.valueOf(Double.NaN)));

        // --- String.format / printf on numbers --------------------------------
        p("f_d", String.format(java.util.Locale.ROOT, "%d", 42));
        p("f_d_neg", String.format(java.util.Locale.ROOT, "%d", Integer.MIN_VALUE));
        p("f_05d", String.format(java.util.Locale.ROOT, "%05d", -42));
        p("f_x", String.format(java.util.Locale.ROOT, "%x", -1));
        p("f_X", String.format(java.util.Locale.ROOT, "%X", 255));
        p("f_o", String.format(java.util.Locale.ROOT, "%o", 8));
        p("f_f", String.format(java.util.Locale.ROOT, "%f", 1.5));
        p("f_f_nan", String.format(java.util.Locale.ROOT, "%f", Double.NaN));
        p("f_f_inf", String.format(java.util.Locale.ROOT, "%f", Double.POSITIVE_INFINITY));
        p("f_2f", String.format(java.util.Locale.ROOT, "%.2f", 2.345));
        p("f_2f_half", String.format(java.util.Locale.ROOT, "%.2f", 2.005));
        p("f_e", String.format(java.util.Locale.ROOT, "%e", 1234.5));
        p("f_g", String.format(java.util.Locale.ROOT, "%g", 1234.5));
        p("f_s_d", String.format(java.util.Locale.ROOT, "%s", 1.0));
        p("f_s_f", String.format(java.util.Locale.ROOT, "%s", 0.1f));
        p("f_10p3f", String.format(java.util.Locale.ROOT, "%10.3f", -1.0 / 3.0));
        p("f_plus", String.format(java.util.Locale.ROOT, "%+d", 5));
        p("f_mz", String.format(java.util.Locale.ROOT, "%.1f", -0.0));

        // --- narrowing/widening through arrays and fields ---------------------
        byte[] ba = new byte[2];
        ba[0] = (byte) 200;
        p("bstore", ba[0]);
        char[] ca = new char[2];
        ca[0] = (char) 70000;
        p("cstore", (int) ca[0]);
        short[] sa = new short[2];
        sa[0] = (short) 40000;
        p("sstore", sa[0]);
        float[] fa = new float[2];
        fa[0] = (float) 0.1;
        p("fstore", fa[0]);
        long[] la = new long[1];
        la[0] = Long.MIN_VALUE;
        p("lstore", la[0]);

        // --- compound assignment does an implicit narrowing cast ---------------
        byte b = 100;
        b += 100;
        p("bcompound", b);
        char c = 'A';
        c += 1000000;
        p("ccompound", (int) c);
        int i = 5;
        i /= 2;
        p("icompound", i);
        int j = 5;
        j += 0.9;
        p("icompound_d", j);
        short sh = 1;
        sh <<= 20;
        p("scompound", sh);
        int k = Integer.MIN_VALUE;
        k >>>= 1;
        p("kcompound", k);

        // --- mixed-type promotion in expressions -------------------------------
        p("promo_1", 1 / 2);
        p("promo_2", 1 / 2.0);
        p("promo_3", 1L + 1);
        p("promo_4", 'a' + 1);
        p("promo_6", 1.0f / 3);
        p("promo_7", (float) (1.0 / 3.0));
        p("promo_8", 1e10f + 1f);
        p("promo_9", (byte) 1 + (byte) 1);
        p("promo_10", Integer.MAX_VALUE + 1L);
        p("promo_11", (long) Integer.MAX_VALUE + 1);
        p("promo_12", 3 * 0.1);
        p("promo_13", 0.1f + 0.2f);
        p("promo_14", 0.1 + 0.2);
        p("promo_15", (double) 0.1f + 0.2);

        // --- ternary numeric promotion (a classic) ------------------------------
        Object o = true ? Integer.valueOf(1) : Double.valueOf(2.0);
        p("ternary", o);
        p("ternary2", true ? 1 : 2.0);

        System.out.println("DONE");
    }
}
