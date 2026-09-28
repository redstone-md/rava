public class StaticCall {
  // Large pure-primitive leaf — sized to exceed the inline budget, so the invokestatic
  // survives to a real call (the register-ABI target).
  static double kernel(double x, double y){
    double a=x, b=y;
    a = a*1.0000001 + 0.5;      b = b*0.9999999 - 0.25;
    a = a - b*0.5 + a*a*0.001;  b = b + a*0.3 - b*b*0.002;
    a = a*1.1 - b*0.7 + a*b*0.01; b = b*0.9 + a*0.4 - a*b*0.02;
    a = a + b - a*0.05;         b = b - a + b*0.06;
    a = a*a*0.0001 + b*0.5;     b = b*b*0.0002 - a*0.25;
    a = a*0.98 + b*0.13 - a*b*0.003; b = b*1.02 - a*0.11 + a*b*0.004;
    return a - b + a*b*0.001 + (a+b)*0.5;
  }
  public static void main(String[] args){
    int n = args.length>0?Integer.parseInt(args[0]):50000000;
    long t0=System.nanoTime();
    double acc=0;
    for(int i=0;i<n;i++) acc += kernel(i*1e-6, acc*1e-9 + 1.0);
    long ms=(System.nanoTime()-t0)/1_000_000;
    System.out.println("StaticCall_ACC="+Double.doubleToLongBits(acc));
    System.out.println("StaticCall_MS="+ms);
  }
}
