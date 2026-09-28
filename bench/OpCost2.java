// SECOND OPERATION SWEEP: string work, collection iteration, arrays. MC does these constantly.
// Non-static sinks throughout (a static sink costs 260-420 ns and would drown everything).
import java.util.*;
public class OpCost2 {
    static Object[] sink = new Object[1]; static int[] isink = new int[1];
    static String a = "minecraft:stone", b = "minecraft:stone", c = "minecraft:dirt";
    static ArrayList<Integer> list = new ArrayList<>();
    static HashMap<String,Integer> map = new HashMap<>();
    static HashSet<String> set = new HashSet<>();
    static int[] arr = new int[256]; static int[] dst = new int[256];
    static { for (int i=0;i<16;i++){ list.add(i); map.put("k"+i,i); set.add("s"+i);} }

    static long strEq(int n){ long r=0; for(int k=0;k<n;k++) r+=a.equals(b)?1:0; return r; }
    static long strNe(int n){ long r=0; for(int k=0;k<n;k++) r+=a.equals(c)?1:0; return r; }
    static long strHash(int n){ long r=0; for(int k=0;k<n;k++) r+=a.hashCode(); return r; }
    static long strCat(int n){ long r=0; for(int k=0;k<n;k++){ sink[0]=a+k; r++; } return r; }
    static long strSub(int n){ long r=0; for(int k=0;k<n;k++){ sink[0]=a.substring(0,9); r++; } return r; }
    static long sbApp(int n){ long r=0; for(int k=0;k<n;k++){ StringBuilder s=new StringBuilder(); s.append('x').append(k).append('y'); sink[0]=s.toString(); r++; } return r; }
    static long listFor(int n){ long r=0; for(int k=0;k<n;k++) for(int i=0;i<list.size();i++) r+=list.get(i); return r; }
    static long listEach(int n){ long r=0; for(int k=0;k<n;k++) for(Integer v:list) r+=v; return r; }
    static long mapEach(int n){ long r=0; for(int k=0;k<n;k++) for(Map.Entry<String,Integer> e:map.entrySet()) r+=e.getValue(); return r; }
    static long setEach(int n){ long r=0; for(int k=0;k<n;k++) for(String s:set) r+=s.length(); return r; }
    static long acopy(int n){ long r=0; for(int k=0;k<n;k++){ System.arraycopy(arr,0,dst,0,256); r++; } return r; }
    static long mathOp(int n){ long r=0; for(int k=0;k<n;k++) r+=(long)Math.sqrt(k)+Math.abs(-k); return r; }
    static long control(int n){ long r=0; for(int k=0;k<n;k++) r+=(k&3); return r; }
    static long run(int w,int n){ switch(w){
        case 0:return strEq(n); case 1:return strNe(n); case 2:return strHash(n); case 3:return strCat(n);
        case 4:return strSub(n); case 5:return sbApp(n); case 6:return listFor(n); case 7:return listEach(n);
        case 8:return mapEach(n); case 9:return setEach(n); case 10:return acopy(n); case 11:return mathOp(n);
        default:return control(n);} }
    static long time(String t,int w,int n,int reps){
        long acc=0; for(int r=0;r<reps;r++) acc+=run(w,n);
        long t0=System.nanoTime(); for(int r=0;r<reps;r++) acc+=run(w,n); long t1=System.nanoTime();
        System.out.println(t+" "+((t1-t0)/((long)n*reps))+" ns/op"); return acc; }
    public static void main(String[] x){
        int n=50000, reps=8; long acc=0;
        String[] nm={"String.equals eq ","String.equals ne ","String.hashCode  ","String concat    ",
                     "String.substring ","StringBuilder x3 ","list index loop  ","list for-each    ",
                     "map entry for-each","set for-each     ","System.arraycopy ","Math.sqrt+abs    ","control          "};
        for(int w=0;w<13;w++) acc+=time(nm[w],w,n,reps);
        System.out.println("acc="+acc);
    }
}
