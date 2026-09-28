public class IntSem {
    static void p(String k, Object v) { System.out.println(k + "=" + v); }

    static int idiv(int a, int b) { return a / b; }
    static int irem(int a, int b) { return a % b; }
    static long ldiv(long a, long b) { return a / b; }
    static long lrem(long a, long b) { return a % b; }

    public static void main(String[] args) {
        // --- division / remainder edge cases -------------------------------
        try { p("idiv_1_0", idiv(1, 0)); }
        catch (ArithmeticException e) { p("idiv_1_0_ex", e.getClass().getName() + ":" + e.getMessage()); }
        try { p("irem_1_0", irem(1, 0)); }
        catch (ArithmeticException e) { p("irem_1_0_ex", e.getClass().getName() + ":" + e.getMessage()); }
        try { p("idiv_0_0", idiv(0, 0)); }
        catch (ArithmeticException e) { p("idiv_0_0_ex", e.getClass().getName() + ":" + e.getMessage()); }
        try { p("ldiv_1_0", ldiv(1L, 0L)); }
        catch (ArithmeticException e) { p("ldiv_1_0_ex", e.getClass().getName() + ":" + e.getMessage()); }
        try { p("lrem_1_0", lrem(1L, 0L)); }
        catch (ArithmeticException e) { p("lrem_1_0_ex", e.getClass().getName() + ":" + e.getMessage()); }

        p("idiv_MIN_m1", idiv(Integer.MIN_VALUE, -1));
        p("irem_MIN_m1", irem(Integer.MIN_VALUE, -1));
        p("ldiv_MIN_m1", ldiv(Long.MIN_VALUE, -1L));
        p("lrem_MIN_m1", lrem(Long.MIN_VALUE, -1L));

        p("idiv_7_m2", idiv(7, -2));
        p("idiv_m7_2", idiv(-7, 2));
        p("irem_7_m2", irem(7, -2));
        p("irem_m7_2", irem(-7, 2));
        p("lrem_m7_2", lrem(-7L, 2L));
        p("ldiv_m7_2", ldiv(-7L, 2L));

        // --- shifts --------------------------------------------------------
        int x = 1;
        p("ishl_1_32", x << 32);
        p("ishl_1_33", x << 33);
        p("ishl_1_m1", x << -1);
        p("ishl_1_31", x << 31);
        p("ishl_1_63", x << 63);
        int n = -8;
        p("ishr_m8_1", n >> 1);
        p("ishr_m8_32", n >> 32);
        p("ishr_m8_33", n >> 33);
        p("ishr_m8_m1", n >> -1);
        p("iushr_m8_1", n >>> 1);
        p("iushr_m8_0", n >>> 0);
        p("iushr_m8_32", n >>> 32);
        p("iushr_m8_33", n >>> 33);
        p("iushr_m8_m1", n >>> -1);
        p("iushr_m1_28", (-1) >>> 28);
        p("ishr_MIN_31", Integer.MIN_VALUE >> 31);
        p("iushr_MIN_31", Integer.MIN_VALUE >>> 31);

        long l = -8L;
        p("lshl_1_64", 1L << 64);
        p("lshl_1_65", 1L << 65);
        p("lshl_1_m1", 1L << -1);
        p("lshl_1_63", 1L << 63);
        p("lshr_m8_1", l >> 1);
        p("lshr_m8_64", l >> 64);
        p("lshr_m8_m1", l >> -1);
        p("lushr_m8_1", l >>> 1);
        p("lushr_m8_64", l >>> 64);
        p("lushr_m8_65", l >>> 65);
        p("lushr_m8_m1", l >>> -1);
        p("lushr_m1_60", (-1L) >>> 60);
        p("lshr_LMIN_63", Long.MIN_VALUE >> 63);
        p("lushr_LMIN_63", Long.MIN_VALUE >>> 63);

        // --- overflow ------------------------------------------------------
        int max = Integer.MAX_VALUE, min = Integer.MIN_VALUE;
        p("iadd_ovf", max + 1);
        p("isub_ovf", min - 1);
        p("imul_ovf", max * 2);
        p("imul_ovf2", min * -1);
        p("ineg_min", -min);
        p("imul_big", 65536 * 65536);
        long lmax = Long.MAX_VALUE, lmin = Long.MIN_VALUE;
        p("ladd_ovf", lmax + 1L);
        p("lsub_ovf", lmin - 1L);
        p("lmul_ovf", lmax * 2L);
        p("lneg_min", -lmin);
        p("lmul_big", 4294967296L * 4294967296L);

        // --- narrowing conversions -----------------------------------------
        int v = 0x1234ABCD;
        p("i2b", (byte) v);
        p("i2c", (int) (char) v);
        p("i2s", (short) v);
        p("i2b_neg", (byte) -1);
        p("i2c_neg", (int) (char) -1);
        p("i2s_neg", (short) -1);
        p("i2b_128", (byte) 128);
        p("i2s_32768", (short) 32768);
        p("l2i", (int) 0x1234567890ABCDEFL);
        p("l2i_neg", (int) -1L);
        p("l2b", (byte) 0x1234567890ABCDEFL);
        p("l2s", (short) 0x1234567890ABCDEFL);
        p("l2c", (int) (char) 0x1234567890ABCDEFL);
        p("i2l_neg", (long) -1);
        p("c2i", (int) 'A');
        char cc = (char) 70000;
        p("char_wrap", (int) cc);

        // --- Math helpers ---------------------------------------------------
        p("floorDiv_7_m2", Math.floorDiv(7, -2));
        p("floorDiv_m7_2", Math.floorDiv(-7, 2));
        p("floorDiv_m7_m2", Math.floorDiv(-7, -2));
        p("floorMod_7_m2", Math.floorMod(7, -2));
        p("floorMod_m7_2", Math.floorMod(-7, 2));
        p("floorMod_m7_m2", Math.floorMod(-7, -2));
        p("floorDiv_MIN_m1", Math.floorDiv(Integer.MIN_VALUE, -1));
        p("floorDivL_m7_2", Math.floorDiv(-7L, 2L));
        p("floorModL_m7_2", Math.floorMod(-7L, 2L));
        p("abs_min", Math.abs(Integer.MIN_VALUE));
        p("absL_min", Math.abs(Long.MIN_VALUE));
        p("abs_m5", Math.abs(-5));
        p("max_i", Math.max(-1, Integer.MIN_VALUE));
        p("min_i", Math.min(-1, Integer.MIN_VALUE));

        try { p("floorMod_1_0", Math.floorMod(1, 0)); }
        catch (ArithmeticException e) { p("floorMod_1_0_ex", e.getClass().getName() + ":" + e.getMessage()); }

        // --- Integer/Long statics -------------------------------------------
        p("Integer_toString_min", Integer.toString(Integer.MIN_VALUE));
        p("Long_toString_min", Long.toString(Long.MIN_VALUE));
        p("Integer_toHexString_m1", Integer.toHexString(-1));
        p("Integer_toBinaryString_m1", Integer.toBinaryString(-1));
        p("Integer_toOctalString_m1", Integer.toOctalString(-1));
        p("Long_toHexString_m1", Long.toHexString(-1L));
        p("Integer_toString_radix", Integer.toString(-255, 16));
        p("Integer_bitCount_m1", Integer.bitCount(-1));
        p("Long_bitCount_m1", Long.bitCount(-1L));
        p("Integer_nlz_0", Integer.numberOfLeadingZeros(0));
        p("Integer_ntz_0", Integer.numberOfTrailingZeros(0));
        p("Long_nlz_0", Long.numberOfLeadingZeros(0L));
        p("Long_ntz_0", Long.numberOfTrailingZeros(0L));
        p("Integer_reverse", Integer.reverse(1));
        p("Integer_reverseBytes", Integer.reverseBytes(1));
        p("Integer_highestOneBit_m1", Integer.highestOneBit(-1));
        p("Integer_lowestOneBit_m1", Integer.lowestOneBit(-1));
        p("Integer_rotateLeft", Integer.rotateLeft(1, -1));
        p("Integer_rotateRight", Integer.rotateRight(1, 33));
        p("Integer_signum", Integer.signum(Integer.MIN_VALUE));
        p("Long_signum", Long.signum(Long.MIN_VALUE));
        p("Integer_compare", Integer.compare(Integer.MIN_VALUE, Integer.MAX_VALUE));
        p("Long_compare", Long.compare(Long.MIN_VALUE, Long.MAX_VALUE));
        p("Integer_divideUnsigned", Integer.divideUnsigned(-1, 3));
        p("Integer_remainderUnsigned", Integer.remainderUnsigned(-1, 3));
        p("Long_divideUnsigned", Long.divideUnsigned(-1L, 3L));
        p("Integer_compareUnsigned", Integer.compareUnsigned(-1, 1));
        p("Integer_toUnsignedLong", Integer.toUnsignedLong(-1));
        p("Integer_toUnsignedString", Integer.toUnsignedString(-1));
        p("Long_toUnsignedString", Long.toUnsignedString(-1L));
        p("Integer_parseInt_min", Integer.parseInt("-2147483648"));
        p("Long_parseLong_min", Long.parseLong("-9223372036854775808"));
        try { p("parseInt_ovf", Integer.parseInt("2147483648")); }
        catch (NumberFormatException e) { p("parseInt_ovf_ex", e.getClass().getName() + ":" + e.getMessage()); }
        try { p("parseInt_empty", Integer.parseInt("")); }
        catch (NumberFormatException e) { p("parseInt_empty_ex", e.getClass().getName() + ":" + e.getMessage()); }

        System.out.println("DONE");
    }
}
