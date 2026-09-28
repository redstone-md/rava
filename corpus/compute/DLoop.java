public class DLoop {
  static double compute(double x, double y, int n){
    double a=x, b=y, s=0;
    for(int i=0;i<n;i++){
      a = a*1.0000001 + 0.5;
      b = b*0.9999999 - 0.25;
      s += a*b - (a+b)*0.5 + Math.sqrt(a*a+1.0);
    }
    return s;
  }
  public static void main(String[] args){
    int iters = args.length>0?Integer.parseInt(args[0]):20000000;
    long t0=System.nanoTime();
    double acc=0;
    for(int r=0;r<10;r++) acc+=compute(1.1+r, 2.2-r, iters);
    long ms=(System.nanoTime()-t0)/1_000_000;
    System.out.println("DLoop_ACC="+Double.doubleToLongBits(acc));
    System.out.println("DLoop_MS="+ms);
  }
}
