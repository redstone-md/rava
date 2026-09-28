// The rows of the JIT-callback census not yet priced. Each is population x cost; only the
// product decides what to fix next.
import java.util.*;
import com.google.common.collect.ImmutableMap;

public class SeamCost3 {
    static Map<String,String> hm = new HashMap<>();
    static ImmutableMap<String,String> im;
    static List<String> list = new ArrayList<>();
    static String k = "k3";
    static Comparable<String> cmp = "abc";
    static StringBuilder sb = new StringBuilder();

    static long entrySetc(int n){ long a=0; for(int i=0;i<n;i++) a += hm.entrySet().size(); return a; }
    static long immGet(int n)   { long a=0; for(int i=0;i<n;i++) a += im.get(k) == null ? 0 : 1; return a; }
    static long cmpEq(int n)    { long a=0; for(int i=0;i<n;i++) a += cmp.equals("abc") ? 1 : 0; return a; }
    static long listSet(int n)  { long a=0; for(int i=0;i<n;i++) { list.set(0, "x"); a += 1; } return a; }
    static long sbApp(int n)    { long a=0; for(int i=0;i<n;i++) { sb.setLength(0); sb.append('x'); a += sb.length(); } return a; }
    static long mapPut(int n)   { long a=0; for(int i=0;i<n;i++) { hm.put(k, "v"); a += 1; } return a; }
    static long mapGetS(int n)  { long a=0; for(int i=0;i<n;i++) a += hm.get(k) == null ? 0 : 1; return a; }
    static long ctl(int n)      { long a=0; for(int i=0;i<n;i++) a += i & 1; return a; }

    static long time(String nm,int w,int n,int reps){
        long best=Long.MAX_VALUE, acc=0;
        for(int r=0;r<reps;r++){ long t0=System.nanoTime();
            long v = switch(w){ case 0->entrySetc(n); case 1->immGet(n); case 2->cmpEq(n);
                                case 3->listSet(n); case 4->sbApp(n); case 5->mapPut(n); case 7->mapGetS(n); default->ctl(n); };
            long dt=System.nanoTime()-t0; acc+=v; if(dt<best) best=dt; }
        System.out.println(nm+" "+(best/n)+" ns/op"); return acc; }

    public static void main(String[] s){
        for(int i=0;i<8;i++){ hm.put("k"+i,"v"+i); list.add("e"+i); }
        im = ImmutableMap.copyOf(hm);
        int n=200_000, reps=6; long acc=0;
        acc+=time("Map.entrySet().size ",0,n,reps);
        acc+=time("ImmutableMap.get    ",1,n,reps);
        acc+=time("Comparable.equals   ",2,n,reps);
        acc+=time("List.set            ",3,n,reps);
        acc+=time("StringBuilder.append",4,n,reps);
        acc+=time("Map.put             ",5,n,reps);
        acc+=time("Map.get(String key)  ",7,n,reps);
        acc+=time("control             ",6,n,reps);
        System.out.println("acc="+acc);
    }
}
