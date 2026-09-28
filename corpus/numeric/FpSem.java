public class FpSem {
    static void p(String k, Object v) { System.out.println(k + "=" + v); }

    public static void main(String[] args) {
        double dn = Double.NaN, dp = Double.POSITIVE_INFINITY, dm = Double.NEGATIVE_INFINITY;
        float fn = Float.NaN, fp = Float.POSITIVE_INFINITY, fm = Float.NEGATIVE_INFINITY;

        // --- NaN comparisons, all six branch forms -------------------------
        p("d_nan_lt", dn < 1.0);
        p("d_nan_le", dn <= 1.0);
        p("d_nan_gt", dn > 1.0);
        p("d_nan_ge", dn >= 1.0);
        p("d_nan_eq", dn == 1.0);
        p("d_nan_ne", dn != 1.0);
        p("d_nan_nan_eq", dn == dn);
        p("d_nan_nan_ne", dn != dn);
        p("f_nan_lt", fn < 1.0f);
        p("f_nan_le", fn <= 1.0f);
        p("f_nan_gt", fn > 1.0f);
        p("f_nan_ge", fn >= 1.0f);
        p("f_nan_eq", fn == 1.0f);
        p("f_nan_ne", fn != 1.0f);

        // --- -0.0 vs 0.0 ----------------------------------------------------
        double z = 0.0, mz = -0.0;
        p("z_eq_mz", z == mz);
        p("z_lt_mz", z < mz);
        p("Double_compare_z_mz", Double.compare(z, mz));
        p("Double_equals_z_mz", Double.valueOf(z).equals(Double.valueOf(mz)));
        p("Float_compare_z_mz", Float.compare(0.0f, -0.0f));
        p("Double_compare_nan_nan", Double.compare(dn, dn));
        p("Double_compare_nan_max", Double.compare(dn, Double.MAX_VALUE));
        p("Double_equals_nan", Double.valueOf(dn).equals(Double.valueOf(dn)));
        p("mz_str", String.valueOf(mz));
        p("mz_f_str", String.valueOf(-0.0f));
        p("mz_add", mz + 0.0);
        p("mz_mul", mz * 1.0);
        p("mz_neg", -0.0 * -1.0);
        p("one_div_mz", 1.0 / mz);
        p("one_div_z", 1.0 / z);

        // --- infinity arithmetic --------------------------------------------
        p("inf_minus_inf", dp - dp);
        p("inf_div_inf", dp / dp);
        p("inf_times_zero", dp * 0.0);
        p("inf_plus_one", dp + 1.0);
        p("one_div_inf", 1.0 / dp);
        p("mone_div_inf", -1.0 / dp);
        p("zero_div_zero", 0.0 / 0.0);
        p("f_zero_div_zero", 0.0f / 0.0f);
        p("f_one_div_zero", 1.0f / 0.0f);
        p("inf_rem", dp % 2.0);
        p("rem_2p5_2", 2.5 % 2.0);
        p("rem_m2p5_2", -2.5 % 2.0);
        p("rem_2p5_m2", 2.5 % -2.0);
        p("frem_m2p5_2", -2.5f % 2.0f);
        p("rem_5_inf", 5.0 % dp);
        p("nan_rem", dn % 2.0);

        // --- f2i / d2i / d2l / f2l saturation and NaN -----------------------
        p("d2i_nan", (int) dn);
        p("d2i_inf", (int) dp);
        p("d2i_minf", (int) dm);
        p("d2i_big", (int) 1e30);
        p("d2i_negbig", (int) -1e30);
        p("d2i_2p5", (int) 2.5);
        p("d2i_m2p5", (int) -2.5);
        p("d2l_nan", (long) dn);
        p("d2l_inf", (long) dp);
        p("d2l_minf", (long) dm);
        p("d2l_big", (long) 1e30);
        p("f2i_nan", (int) fn);
        p("f2i_inf", (int) fp);
        p("f2i_minf", (int) fm);
        p("f2l_inf", (long) fp);
        p("f2l_nan", (long) fn);
        p("d2f_big", (float) 1e300);
        p("d2f_small", (float) 1e-300);
        p("d2f_nan", (float) dn);
        p("f2d_01", (double) 0.1f);
        p("f2d_3", (double) 3.0f);
        p("f2d_1e7", (double) 1e7f);
        p("i2f_big", (float) 16777217);
        p("l2f_big", (float) 9223372036854775807L);
        p("l2d_big", (double) 9223372036854775807L);
        p("i2d", (double) 123456789);

        // --- printing -------------------------------------------------------
        p("s_0p1f", String.valueOf(0.1f));
        p("s_1e7f", String.valueOf(1e7f));
        p("s_1em5", String.valueOf(1.0E-5));
        p("s_3p0", String.valueOf(3.0));
        p("s_100p0", String.valueOf(100.0));
        p("s_nan", String.valueOf(dn));
        p("s_inf", String.valueOf(dp));
        p("s_minf", String.valueOf(dm));
        p("s_fnan", String.valueOf(fn));
        p("s_finf", String.valueOf(fp));
        p("s_dmax", String.valueOf(Double.MAX_VALUE));
        p("s_dmin", String.valueOf(Double.MIN_VALUE));
        p("s_dminnorm", String.valueOf(Double.MIN_NORMAL));
        p("s_fmax", String.valueOf(Float.MAX_VALUE));
        p("s_fmin", String.valueOf(Float.MIN_VALUE));
        p("s_fminnorm", String.valueOf(Float.MIN_NORMAL));
        p("s_1e_3", String.valueOf(0.001));
        p("s_1e_4", String.valueOf(0.0001));
        p("s_9999999", String.valueOf(9999999.0));
        p("s_1e7", String.valueOf(1.0E7));
        p("s_1e7m1", String.valueOf(9999999.999));
        p("s_123456789", String.valueOf(123456789.0));
        p("s_1_3", String.valueOf(1.0 / 3.0));
        p("s_2_3", String.valueOf(2.0 / 3.0));
        p("s_1_3f", String.valueOf(1.0f / 3.0f));
        p("s_pi", String.valueOf(Math.PI));
        p("s_e", String.valueOf(Math.E));
        p("s_pif", String.valueOf((float) Math.PI));
        p("s_neg", String.valueOf(-1.5));
        p("s_negsmall", String.valueOf(-1.0E-10));
        p("s_1e20", String.valueOf(1.0E20));
        p("s_1e23", String.valueOf(1.0E23));
        p("s_2e23", String.valueOf(2.0E23));
        p("s_1p1e22", String.valueOf(1.1E22));
        p("s_9e_4", String.valueOf(9.0E-4));
        p("s_1234p5678", String.valueOf(1234.5678));
        p("s_0p5", String.valueOf(0.5f));
        p("s_1e10f", String.valueOf(1.0E10f));
        p("s_1e_10f", String.valueOf(1.0E-10f));
        p("s_16777216f", String.valueOf(16777216.0f));
        p("s_1p4e_45f", String.valueOf(1.4E-45f));
        p("s_3p4e38f", String.valueOf(3.4028235E38f));
        p("s_bytemax", String.valueOf(Byte.MAX_VALUE));
        p("s_charA", String.valueOf('A'));
        p("s_true", String.valueOf(true));
        p("s_longmin", String.valueOf(Long.MIN_VALUE));
        p("s_intmin", String.valueOf(Integer.MIN_VALUE));
        p("concat_d", "" + 0.1 + 1e7f + (-0.0));

        // --- bit conversions -------------------------------------------------
        p("dtolb_nan", Double.doubleToLongBits(dn));
        p("dtorlb_nan", Double.doubleToRawLongBits(dn));
        p("dtolb_mz", Double.doubleToLongBits(-0.0));
        p("dtolb_1", Double.doubleToLongBits(1.0));
        p("ltod", Double.longBitsToDouble(0x3FF0000000000000L));
        p("ftoib_nan", Float.floatToIntBits(fn));
        p("ftoib_mz", Float.floatToIntBits(-0.0f));
        p("itof", Float.intBitsToFloat(0x3F800000));
        p("isNaN_d", Double.isNaN(dn));
        p("isInfinite_d", Double.isInfinite(dm));
        p("isFinite_d", Double.isFinite(dn));

        // --- parsing ---------------------------------------------------------
        p("parseD_1", Double.parseDouble("1"));
        p("parseD_1e5", Double.parseDouble("1e5"));
        p("parseD_nan", Double.parseDouble("NaN"));
        p("parseD_inf", Double.parseDouble("Infinity"));
        p("parseD_mz", Double.parseDouble("-0.0"));
        p("parseF", Float.parseFloat("0.1"));

        // --- Math on doubles ---------------------------------------------------
        p("m_round_2p5", Math.round(2.5));
        p("m_round_m2p5", Math.round(-2.5));
        p("m_round_nan", Math.round(dn));
        p("m_round_inf", Math.round(dp));
        p("m_round_f", Math.round(2.5f));
        p("m_round_edge", Math.round(0.49999999999999994));
        p("m_rint_2p5", Math.rint(2.5));
        p("m_rint_m2p5", Math.rint(-2.5));
        p("m_rint_0p5", Math.rint(0.5));
        p("m_floor_m0p5", Math.floor(-0.5));
        p("m_ceil_m0p5", Math.ceil(-0.5));
        p("m_abs_mz", Math.abs(-0.0));
        p("m_abs_nan", Math.abs(dn));
        p("m_min_z_mz", Math.min(0.0, -0.0));
        p("m_max_z_mz", Math.max(0.0, -0.0));
        p("m_min_nan", Math.min(dn, 1.0));
        p("m_max_nan", Math.max(dn, 1.0));
        p("m_sqrt_m1", Math.sqrt(-1.0));
        p("m_sqrt_mz", Math.sqrt(-0.0));
        p("m_pow_1_nan", Math.pow(1.0, dn));
        p("m_pow_nan_0", Math.pow(dn, 0.0));
        p("m_pow_m1_inf", Math.pow(-1.0, dp));
        p("m_pow_2_10", Math.pow(2.0, 10.0));
        p("m_signum_mz", Math.signum(-0.0));
        p("m_signum_nan", Math.signum(dn));
        p("m_ulp_1", Math.ulp(1.0));
        p("m_ulp_0", Math.ulp(0.0));
        p("m_ulp_inf", Math.ulp(dp));
        p("m_ulp_nan", Math.ulp(dn));
        p("m_getExponent_1", Math.getExponent(1.0));
        p("m_getExponent_0", Math.getExponent(0.0));
        p("m_getExponent_nan", Math.getExponent(dn));
        p("m_getExponent_inf", Math.getExponent(dp));
        p("m_getExponent_p5", Math.getExponent(0.5));
        p("m_scalb", Math.scalb(1.0, 3));
        p("m_scalb_big", Math.scalb(1.0, 2000));
        p("m_scalb_small", Math.scalb(1.0, -2000));
        p("m_ieee_0p5", Math.IEEEremainder(0.5, 1.0));
        p("m_ieee_1p5", Math.IEEEremainder(1.5, 1.0));
        p("m_ieee_2p5", Math.IEEEremainder(2.5, 1.0));
        p("m_copySign", Math.copySign(1.0, -0.0));
        p("m_nextUp_0", Math.nextUp(0.0));
        p("m_nextDown_0", Math.nextDown(0.0));
        p("m_hypot", Math.hypot(3.0, 4.0));
        p("m_cbrt_m8", Math.cbrt(-8.0));
        p("m_log_0", Math.log(0.0));
        p("m_log_m1", Math.log(-1.0));
        p("m_exp_inf", Math.exp(dp));
        p("m_toIntExact_ok", Math.toIntExact(5L));
        try { p("m_toIntExact_ovf", Math.toIntExact(1L << 40)); }
        catch (ArithmeticException e) { p("m_toIntExact_ovf_ex", e.getClass().getName() + ":" + e.getMessage()); }
        try { p("m_addExact_ovf", Math.addExact(Integer.MAX_VALUE, 1)); }
        catch (ArithmeticException e) { p("m_addExact_ovf_ex", e.getClass().getName() + ":" + e.getMessage()); }
        try { p("m_addExactL_ovf", Math.addExact(Long.MAX_VALUE, 1L)); }
        catch (ArithmeticException e) { p("m_addExactL_ovf_ex", e.getClass().getName() + ":" + e.getMessage()); }
        try { p("m_mulExact_ovf", Math.multiplyExact(Integer.MAX_VALUE, 2)); }
        catch (ArithmeticException e) { p("m_mulExact_ovf_ex", e.getClass().getName() + ":" + e.getMessage()); }
        try { p("m_negExact_ovf", Math.negateExact(Integer.MIN_VALUE)); }
        catch (ArithmeticException e) { p("m_negExact_ovf_ex", e.getClass().getName() + ":" + e.getMessage()); }
        try { p("m_subExact_ovf", Math.subtractExact(Integer.MIN_VALUE, 1)); }
        catch (ArithmeticException e) { p("m_subExact_ovf_ex", e.getClass().getName() + ":" + e.getMessage()); }
        p("m_addExactL_ok", Math.addExact(1234567890123456789L, 1L));
        p("m_mulExactL_ok", Math.multiplyExact(1234567890L, 1000000L));
        p("m_clamp", Math.clamp(5L, 1, 3));
        p("m_clamp2", Math.clamp(1234567890123456789L, 1L, 9223372036854775807L));

        System.out.println("DONE");
    }
}
