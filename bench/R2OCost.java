// THE BOOT'S #1 BRIDGE SITE, in isolation. `[BRIDGE-CALLER]` puts
// `StateHolder @cp199 -> java/util/Map.get` at 38.6% of ALL native-bridge time in the
// bootstrap phase, and the receiver there is a fastutil Reference2ObjectArrayMap:
// identity-keyed, array-backed, a handful of entries. `get` on that is a pointer scan.
import it.unimi.dsi.fastutil.objects.Reference2ObjectArrayMap;
import java.util.Map;
public class R2OCost {
    static Object[] props = new Object[6];
    static Reference2ObjectArrayMap<Object,Object> r2o = new Reference2ObjectArrayMap<>();
    static Map<Object,Object> asMap;                 // the DECLARED type at the hot site
    static long r2oGet(int n){ long r=0; for(int i=0;i<n;i++) r += r2o.get(props[i%6])==null?0:1; return r; }
    static long mapGet(int n){ long r=0; for(int i=0;i<n;i++) r += asMap.get(props[i%6])==null?0:1; return r; }
    static long control(int n){ long r=0; for(int i=0;i<n;i++) r += (i%6); return r; }
    static long run(int w,int n){ switch(w){case 0:return r2oGet(n);case 1:return mapGet(n);default:return control(n);} }
    static long time(String t,int w,int n,int reps){
        long acc=0; for(int r=0;r<reps;r++) acc+=run(w,n);
        long t0=System.nanoTime(); for(int r=0;r<reps;r++) acc+=run(w,n); long t1=System.nanoTime();
        System.out.println(t+" "+((t1-t0)/((long)n*reps))+" ns/call"); return acc; }
    public static void main(String[] a){
        for(int i=0;i<6;i++){ props[i]=new Object(); r2o.put(props[i], new Object()); }
        asMap = r2o;
        long acc=0; int n=100000, reps=12;
        acc+=time("R2OMap.get (exact type)  ",0,n,reps);
        acc+=time("Map.get   (declared Map) ",1,n,reps);
        acc+=time("control                  ",2,n,reps);
        System.out.println("acc="+acc);
    }
}
