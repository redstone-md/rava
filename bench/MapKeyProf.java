// PROFILE TARGET: Map.get with an OBJECT key. The Integer-key control in NativeCost
// costs 66 ns net on the same cascade; this costs 614. The difference is KEY DERIVATION,
// and this loop contains nothing else.
import java.util.*;
public class MapKeyProf {
    static HashMap<Object,Object> m = new HashMap<>();
    static Object[] k = new Object[4];
    static long g(int n){ long r=0; for(int i=0;i<n;i++) r += m.get(k[i&3])==null?0:1; return r; }
    public static void main(String[] a){
        for(int i=0;i<4;i++){ k[i]=new Object(); m.put(k[i], new Object()); }
        long acc=0; for(int w=0;w<3;w++) acc+=g(200000);
        long t0=System.nanoTime(); for(int r=0;r<40;r++) acc+=g(200000); long t1=System.nanoTime();
        System.out.println("map.get "+((t1-t0)/(200000L*40))+" ns/call acc="+acc);
    }
}
