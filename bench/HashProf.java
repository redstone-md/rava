// PROFILE TARGET: identity hashCode — a header-word read on the JVM (1 ns), 1,166 ns here.
public class HashProf {
    static Object o = new Object();
    static long h(int n){ long r=0; for(int i=0;i<n;i++) r+=o.hashCode(); return r; }
    public static void main(String[] a){
        long acc=0; for(int w=0;w<3;w++) acc+=h(200000);
        long t0=System.nanoTime(); for(int r=0;r<40;r++) acc+=h(200000); long t1=System.nanoTime();
        System.out.println("hashCode "+((t1-t0)/(200000L*40))+" ns/call acc="+acc);
    }
}
