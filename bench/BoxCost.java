// Integer.valueOf: the JVM MUST cache -128..127 (JLS 5.1.7), so an in-range box is free and an
// out-of-range one allocates. If rava allocates for both, boxing is a per-call allocation on a
// path Java takes constantly — and `mi_malloc_aligned` is the boot profile's top row.
public class BoxCost {
    static Object[] sink = new Object[1];
    static long inRange(int n){ long r=0; for(int k=0;k<n;k++){ sink[0]=Integer.valueOf(k&127); r++; } return r; }
    static long outRange(int n){ long r=0; for(int k=0;k<n;k++){ sink[0]=Integer.valueOf(100000+(k&127)); r++; } return r; }
    static long identity(int n){ long r=0; for(int k=0;k<n;k++){ if(Integer.valueOf(7)==Integer.valueOf(7)) r++; } return r; }
    static long longIn(int n){ long r=0; for(int k=0;k<n;k++){ sink[0]=Long.valueOf(k&127); r++; } return r; }
    static long charIn(int n){ long r=0; for(int k=0;k<n;k++){ sink[0]=Character.valueOf((char)(k&127)); r++; } return r; }
    static long boolBox(int n){ long r=0; for(int k=0;k<n;k++){ sink[0]=Boolean.valueOf((k&1)==0); r++; } return r; }
    static long control(int n){ long r=0; for(int k=0;k<n;k++) r+=(k&3); return r; }
    static long run(int w,int n){ switch(w){
        case 0:return inRange(n); case 1:return outRange(n); case 2:return identity(n);
        case 3:return longIn(n); case 4:return charIn(n); case 5:return boolBox(n); default:return control(n);} }
    static long time(String t,int w,int n,int reps){
        long acc=0; for(int r=0;r<reps;r++) acc+=run(w,n);
        long t0=System.nanoTime(); for(int r=0;r<reps;r++) acc+=run(w,n); long t1=System.nanoTime();
        System.out.println(t+" "+((t1-t0)/((long)n*reps))+" ns/op"); return acc; }
    public static void main(String[] a){
        int n=200000, reps=8; long acc=0;
        String[] nm={"Integer.valueOf in-cache ","Integer.valueOf out-cache","Integer identity in-cache",
                     "Long.valueOf in-cache    ","Character.valueOf        ","Boolean.valueOf          ","control                  "};
        for(int w=0;w<7;w++) acc+=time(nm[w],w,n,reps);
        System.out.println("acc="+acc);
    }
}
