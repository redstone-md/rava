public class Mega {
  interface F { long f(long x); }
  static final class K0 implements F { public long f(long x){ return x*1+0; } }
  static final class K1 implements F { public long f(long x){ return x*3+1; } }
  static final class K2 implements F { public long f(long x){ return x*5+2; } }
  static final class K3 implements F { public long f(long x){ return x*7+3; } }
  static final class K4 implements F { public long f(long x){ return x*9+4; } }
  static final class K5 implements F { public long f(long x){ return x*11+5; } }
  static final class K6 implements F { public long f(long x){ return x*13+6; } }
  static final class K7 implements F { public long f(long x){ return x*15+7; } }
  static final class K8 implements F { public long f(long x){ return x*17+8; } }
  static final class K9 implements F { public long f(long x){ return x*19+9; } }
  static final class K10 implements F { public long f(long x){ return x*21+10; } }
  static final class K11 implements F { public long f(long x){ return x*23+11; } }
  static final class K12 implements F { public long f(long x){ return x*25+12; } }
  static final class K13 implements F { public long f(long x){ return x*27+13; } }
  static final class K14 implements F { public long f(long x){ return x*29+14; } }
  static final class K15 implements F { public long f(long x){ return x*31+15; } }
  static final class K16 implements F { public long f(long x){ return x*33+16; } }
  static final class K17 implements F { public long f(long x){ return x*35+17; } }
  static final class K18 implements F { public long f(long x){ return x*37+18; } }
  static final class K19 implements F { public long f(long x){ return x*39+19; } }

  public static void main(String[] a){
    int n=Integer.parseInt(a[0]);
    F[] fs = { new K0(),new K1(),new K2(),new K3(),new K4(),new K5(),new K6(),new K7(),new K8(),new K9(),new K10(),new K11(),new K12(),new K13(),new K14(),new K15(),new K16(),new K17(),new K18(),new K19() };
    long acc=0;
    for(int i=0;i<n;i++){ acc += fs[i%20].f(acc ^ i); }
    System.out.println(acc);
  }
}