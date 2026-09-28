// Boundary-dense: per-element virtual call returning double (mimics density tree node).
public class VCall {
  interface DF { double compute(double x); }
  static final class Add implements DF { final DF a,b; Add(DF a,DF b){this.a=a;this.b=b;} public double compute(double x){return a.compute(x)+b.compute(x);} }
  static final class Mul implements DF { final DF a,b; Mul(DF a,DF b){this.a=a;this.b=b;} public double compute(double x){return a.compute(x)*b.compute(x);} }
  static final class Con implements DF { final double c; Con(double c){this.c=c;} public double compute(double x){return c+x*0.0;} }
  static final class Xin implements DF { public double compute(double x){return x;} }
  static DF tree(){ DF x=new Xin(); DF c=new Con(1.5);
    return new Add(new Mul(x, new Add(c, x)), new Mul(new Con(0.3), new Add(x, new Mul(c,x)))); }
  public static void main(String[] a){
    int n=a.length>0?Integer.parseInt(a[0]):20000000;
    DF t=tree(); long t0=System.nanoTime(); double s=0;
    for(int i=0;i<n;i++) s+=t.compute(i*1e-6);
    long ms=(System.nanoTime()-t0)/1_000_000;
    System.out.println("VCall_ACC="+Double.doubleToLongBits(s));
    System.out.println("VCall_MS="+ms);
  }
}
