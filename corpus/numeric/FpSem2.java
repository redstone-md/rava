public class FpSem2 {
    static void p(String k, Object v) { System.out.println(k + "=" + v); }

    public static void main(String[] args) {
        // long-precision integer helpers that must NOT roundtrip through double
        p("floorDivL", Math.floorDiv(1234567890123456789L, 7L));
        p("floorModL", Math.floorMod(1234567890123456789L, 7L));
        p("floorDivL_neg", Math.floorDiv(-1234567890123456789L, 7L));
        p("floorModL_neg", Math.floorMod(-1234567890123456789L, 7L));
        p("floorDivL_MIN_m1", Math.floorDiv(Long.MIN_VALUE, -1L));
        p("floorModL_MIN_m1", Math.floorMod(Long.MIN_VALUE, -1L));
        p("floorMod_int_long", Math.floorMod(-7L, 2));
        try { p("floorDivL_0", Math.floorDiv(1L, 0L)); }
        catch (ArithmeticException e) { p("floorDivL_0_ex", e.getMessage()); }
        p("multiplyHigh", Math.multiplyHigh(1L << 62, 4L));
        p("ceilDiv", Math.ceilDiv(7, 2));
        p("ceilMod", Math.ceilMod(7, 2));
        p("absExact", Math.absExact(-5));

        // float-typed Math overloads
        p("ulp_f", Math.ulp(1.0f));
        p("ulp_f0", Math.ulp(0.0f));
        p("ulp_finf", Math.ulp(Float.POSITIVE_INFINITY));
        p("ulp_fmax", Math.ulp(Float.MAX_VALUE));
        p("ulp_dmax", Math.ulp(Double.MAX_VALUE));
        p("nextUp_f", Math.nextUp(1.0f));
        p("nextDown_f", Math.nextDown(1.0f));
        p("nextAfter_d", Math.nextAfter(1.0, 2.0));
        p("getExponent_f", Math.getExponent(1.0f));
        p("getExponent_f0", Math.getExponent(0.0f));
        p("getExponent_dsub", Math.getExponent(Double.MIN_VALUE));
        p("getExponent_dminnorm", Math.getExponent(Double.MIN_NORMAL));
        p("getExponent_1023", Math.getExponent(Double.MAX_VALUE));
        p("getExponent_3", Math.getExponent(3.0));
        p("signum_f_mz", Math.signum(-0.0f));
        p("signum_f_z", Math.signum(0.0f));
        p("abs_f_mz", Math.abs(-0.0f));
        p("fma_f", Math.fma(2.0f, 3.0f, 4.0f));
        p("scalb_f", Math.scalb(1.0f, 3));
        p("copySign_f", Math.copySign(1.0f, -2.0f));
        p("pow_neg", Math.pow(-8.0, 1.0 / 3.0));
        p("pow_1_inf", Math.pow(1.0, Double.POSITIVE_INFINITY));
        p("pow_m1_minf", Math.pow(-1.0, Double.NEGATIVE_INFINITY));
        p("pow_0_0", Math.pow(0.0, 0.0));
        p("pow_mz_3", Math.pow(-0.0, 3.0));
        p("pow_m2_3", Math.pow(-2.0, 3.0));
        p("pow_m2_3p5", Math.pow(-2.0, 3.5));
        p("ieee_m0p5", Math.IEEEremainder(-0.5, 1.0));
        p("ieee_3p5", Math.IEEEremainder(3.5, 1.0));
        p("ieee_inf", Math.IEEEremainder(1.0, Double.POSITIVE_INFINITY));

        // toUnsignedString with radix, compareUnsigned family
        p("cu_i", Integer.compareUnsigned(-1, 1));
        p("cu_i2", Integer.compareUnsigned(1, -1));
        p("cu_i3", Integer.compareUnsigned(3, 3));
        p("cu_l", Long.compareUnsigned(-1L, 1L));
        p("us_i", Integer.toUnsignedString(-1));
        p("us_i16", Integer.toUnsignedString(-1, 16));
        p("us_l", Long.toUnsignedString(-1L));
        p("us_l16", Long.toUnsignedString(-1L, 16));
        p("ts_i16", Integer.toString(-255, 16));
        p("ts_i1", Integer.toString(255, 1));
        p("ts_i37", Integer.toString(255, 37));
        p("pui", Integer.parseUnsignedInt("4294967295"));
        p("pul", Long.parseUnsignedLong("18446744073709551615"));
        p("divU_0chk", Integer.divideUnsigned(7, 2));

        System.out.println("DONE");
    }
}
