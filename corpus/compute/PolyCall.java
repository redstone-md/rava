public class PolyCall {
  interface F { long f(long x); }
  static final class A implements F { public long f(long x){ return x*3+1; } }
  static final class B implements F { public long f(long x){ return x^0x5bd1; } }
  static final class C implements F { public long f(long x){ return x+7; } }
  static final class D implements F { public long f(long x){ return x*31; } }
  public static void main(String[] a){
    int n=Integer.parseInt(a[0]);
    F[] fs = { new A(), new B(), new C(), new D() };
    long acc=0;
    for(int i=0;i<n;i++){ acc += fs[i&3].f(acc ^ i); }  // polymorphic virtual dispatch per iter
    System.out.println(acc);
  }
}
