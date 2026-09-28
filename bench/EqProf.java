// PROFILE TARGET: one native bridge call that does NOTHING (null == null), in a tight
// tiered-up loop. Whatever the host profile shows here IS the native calling convention,
// with no body to hide behind.
public class EqProf {
    static Object x, y;
    static long eq(int n){ long r=0; for(int i=0;i<n;i++) r += java.util.Objects.equals(x,y)?1:0; return r; }
    public static void main(String[] s){
        long acc=0;
        for(int w=0;w<3;w++) acc+=eq(200000);
        long t0=System.nanoTime();
        for(int r=0;r<40;r++) acc+=eq(200000);
        long t1=System.nanoTime();
        System.out.println("eq "+((t1-t0)/(200000L*40))+" ns/call acc="+acc);
    }
}
