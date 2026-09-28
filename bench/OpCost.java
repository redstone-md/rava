// OPERATION SWEEP vs the JVM. The tiering defect was found this way, not by profiling: a
// 310 ns virtual call next to a 1 ns static one. Every loop uses a NON-static sink, because a
// static sink costs 260-420 ns and drowns whatever it is meant to measure.
public class OpCost {
    interface I { default int d(){ return 7; } int v(); }
    static class A implements I { int f=1; public int v(){ return f; } }
    static int[] ia = new int[64]; static Object[] oa = new Object[64];
    static long[] la = new long[64]; static double[] da = new double[64];
    static A a = new A(); static I i = a; static Object obj = a;
    static int[] sink = new int[1]; static Object[] osink = new Object[1];
    static String s = "abcdefghij"; static StringBuilder sb = new StringBuilder();

    static long arrGetI(int n){ long r=0; for(int k=0;k<n;k++) r+=ia[k&63]; return r; }
    static long arrPutI(int n){ long r=0; for(int k=0;k<n;k++){ ia[k&63]=k; r++; } return r; }
    static long arrGetO(int n){ long r=0; for(int k=0;k<n;k++){ osink[0]=oa[k&63]; r++; } return r; }
    static long arrPutO(int n){ long r=0; for(int k=0;k<n;k++){ oa[k&63]=a; r++; } return r; }
    static long arrGetD(int n){ long r=0; for(int k=0;k<n;k++) r+=(long)da[k&63]; return r; }
    static long longArith(int n){ long r=0; for(int k=0;k<n;k++) r+=((long)k*3)^((long)k>>2); return r; }
    static long dblArith(int n){ double d=0; for(int k=0;k<n;k++) d+=k*1.5; return (long)d; }
    static long ifaceDefault(int n){ long r=0; for(int k=0;k<n;k++) r+=i.d(); return r; }
    static long instOf(int n){ long r=0; for(int k=0;k<n;k++) r+=(obj instanceof A)?1:0; return r; }
    static long castOp(int n){ long r=0; for(int k=0;k<n;k++){ osink[0]=(A)obj; r++; } return r; }
    static long boxInt(int n){ long r=0; for(int k=0;k<n;k++){ osink[0]=Integer.valueOf(k&127); r++; } return r; }
    static long strLen(int n){ long r=0; for(int k=0;k<n;k++) r+=s.length(); return r; }
    static long strChar(int n){ long r=0; for(int k=0;k<n;k++) r+=s.charAt(k&9); return r; }
    static long lambda(int n){ java.util.function.IntUnaryOperator f=x->x+1; long r=0; for(int k=0;k<n;k++) r+=f.applyAsInt(k); return r; }
    static long tryCatch(int n){ long r=0; for(int k=0;k<n;k++){ try { r+=ia[k&63]; } catch(RuntimeException e){ r--; } } return r; }
    static long sync(int n){ long r=0; for(int k=0;k<n;k++){ synchronized(a){ r++; } } return r; }
    static long control(int n){ long r=0; for(int k=0;k<n;k++) r+=(k&3); return r; }
    static long run(int w,int n){ switch(w){
        case 0:return arrGetI(n); case 1:return arrPutI(n); case 2:return arrGetO(n); case 3:return arrPutO(n);
        case 4:return arrGetD(n); case 5:return longArith(n); case 6:return dblArith(n); case 7:return ifaceDefault(n);
        case 8:return instOf(n); case 9:return castOp(n); case 10:return boxInt(n); case 11:return strLen(n);
        case 12:return strChar(n); case 13:return lambda(n); case 14:return tryCatch(n); case 15:return sync(n);
        default:return control(n);} }
    static long time(String t,int w,int n,int reps){
        long acc=0; for(int r=0;r<reps;r++) acc+=run(w,n);
        long t0=System.nanoTime(); for(int r=0;r<reps;r++) acc+=run(w,n); long t1=System.nanoTime();
        System.out.println(t+" "+((t1-t0)/((long)n*reps))+" ns/op"); return acc; }
    public static void main(String[] x){
        int n=200000, reps=8; long acc=0;
        String[] nm={"array get int ","array put int ","array get ref ","array put ref ","array get dbl ",
                     "long arith    ","double arith  ","iface default ","instanceof    ","checkcast     ",
                     "Integer.valueOf","String.length ","String.charAt ","lambda apply  ","try/catch     ",
                     "synchronized  ","control       "};
        for(int w=0;w<17;w++) acc+=time(nm[w],w,n,reps);
        System.out.println("acc="+acc);
    }
}
