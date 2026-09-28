// STATIC FIELD ACCESS vs INSTANCE FIELD ACCESS. A profile of a ref-returning call put
// putstatic+getstatic+getstatic_resolve+get_or_load+HashMap::insert<String,..> at ~60% of a
// 282 ns op — i.e. a static access re-resolving by NAME, through the class loader, every time.
public class StaticField {
    static int si; static Object so; static final Object CONST = new Object();
    static class H { int i; Object o; }
    static H h = new H();
    static long sGet(int n){ long r=0; for(int k=0;k<n;k++) r+=si; return r; }
    static long sPut(int n){ long r=0; for(int k=0;k<n;k++){ si=k; r++; } return r; }
    static long sGetRef(int n){ long r=0; for(int k=0;k<n;k++){ if(CONST!=null) r++; } return r; }
    static long sPutRef(int n){ long r=0; for(int k=0;k<n;k++){ so=CONST; r++; } return r; }
    static long iGet(int n){ long r=0; for(int k=0;k<n;k++) r+=h.i; return r; }
    static long iPut(int n){ long r=0; for(int k=0;k<n;k++){ h.i=k; r++; } return r; }
    static long iPutRef(int n){ long r=0; for(int k=0;k<n;k++){ h.o=CONST; r++; } return r; }
    static long control(int n){ long r=0; for(int k=0;k<n;k++) r+=(k&3); return r; }
    static long run(int w,int n){ switch(w){
        case 0:return sGet(n); case 1:return sPut(n); case 2:return sGetRef(n); case 3:return sPutRef(n);
        case 4:return iGet(n); case 5:return iPut(n); case 6:return iPutRef(n); default:return control(n);} }
    static long time(String t,int w,int n,int reps){
        long acc=0; for(int r=0;r<reps;r++) acc+=run(w,n);
        long t0=System.nanoTime(); for(int r=0;r<reps;r++) acc+=run(w,n); long t1=System.nanoTime();
        System.out.println(t+" "+((t1-t0)/((long)n*reps))+" ns/op"); return acc; }
    public static void main(String[] a){
        int n=300000, reps=8; long acc=0;
        String[] nm={"static get int","static put int","static get ref","static put ref",
                     "inst   get int","inst   put int","inst   put ref","control       "};
        for(int w=0;w<8;w++) acc+=time(nm[w],w,n,reps);
        System.out.println("acc="+acc);
    }
}
