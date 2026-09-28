// Is a STATIC native's dispatch the cost? Same harness shape as NativeCost:
// each loop in its own method, called twice, so it tiers up.
public class StaticCost {
    static Object x, y;
    static int a = 3, b = 7;
    static long objEquals(int n){ long r=0; for(int i=0;i<n;i++) r += java.util.Objects.equals(x,y)?1:0; return r; }
    static long mathMax(int n)  { long r=0; for(int i=0;i<n;i++) r += Math.max(a,b); return r; }
    static long reqNonNull(int n){ long r=0; for(int i=0;i<n;i++) r += java.util.Objects.requireNonNull(x,"m")==null?0:1; return r; }
    static long isNull(int n)   { long r=0; for(int i=0;i<n;i++) r += java.util.Objects.isNull(x)?1:0; return r; }
    static long control(int n)  { long r=0; for(int i=0;i<n;i++) r += (i&3); return r; }
    static long run(int w,int n){ switch(w){case 0:return objEquals(n);case 1:return mathMax(n);case 2:return reqNonNull(n);case 3:return isNull(n);default:return control(n);} }
    static long time(String t,int w,int n,int reps){
        long acc=0; for(int r=0;r<reps;r++) acc+=run(w,n);
        long t0=System.nanoTime(); for(int r=0;r<reps;r++) acc+=run(w,n); long t1=System.nanoTime();
        System.out.println(t+" "+((t1-t0)/((long)n*reps))+" ns/call"); return acc; }
    public static void main(String[] s){
        int n=200000, reps=5; long acc=0;
        acc+=time("Objects.equals(null,null)",0,n,reps);
        acc+=time("Math.max(int,int)       ",1,n,reps);
        acc+=time("Objects.requireNonNull  ",2,n,reps);
        acc+=time("Objects.isNull          ",3,n,reps);
        acc+=time("control loop            ",4,n,reps);
        System.out.println("acc="+acc);
    }
}
