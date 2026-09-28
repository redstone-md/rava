// PER-NATIVE COST, on the population the boot's [WALL-BRIDGE] tables actually name.
// Same discipline as NativeCost: each loop in its own method, warmed, then timed, so
// every number is a COMPILED number. Subtract `control` for the loop itself.
import java.util.*;
public class BridgeCost {
    static ArrayList<Object> list = new ArrayList<>();
    static HashMap<Object,Object> map = new HashMap<>();
    static Object[] keys = new Object[4];
    static Object o1 = new Object(), o2 = new Object();
    static String s = "net/minecraft/world/level/block/Block";
    static int sink;
    static long listSize(int n){ long r=0; for(int i=0;i<n;i++) r+=list.size(); return r; }
    static long listGet(int n) { long r=0; for(int i=0;i<n;i++) r+=list.get(i&3)==null?0:1; return r; }
    static long listSet(int n) { long r=0; for(int i=0;i<n;i++){ list.set(i&3,o1); r++; } return r; }
    static long listIter(int n){ long r=0; for(int i=0;i<n;i++){ Iterator<Object> it=list.iterator(); if(it.hasNext()){it.next(); r++;} } return r; }
    static long mapGet(int n)  { long r=0; for(int i=0;i<n;i++) r+=map.get(keys[i&3])==null?0:1; return r; }
    static long mapPut(int n)  { long r=0; for(int i=0;i<n;i++){ map.put(keys[i&3],o1); r++; } return r; }
    static long mapEntry(int n){ long r=0; for(int i=0;i<n;i++) r+=map.entrySet().size(); return r; }
    static long objEq(int n)   { long r=0; for(int i=0;i<n;i++) r+=o1.equals(o2)?1:0; return r; }
    static long objsEq(int n)  { long r=0; for(int i=0;i<n;i++) r+=Objects.equals(o1,o2)?1:0; return r; }
    static long strLen(int n)  { long r=0; for(int i=0;i<n;i++) r+=s.length(); return r; }
    static long strIdx(int n)  { long r=0; for(int i=0;i<n;i++) r+=s.indexOf('/'); return r; }
    static long hash(int n)    { long r=0; for(int i=0;i<n;i++) r+=o1.hashCode(); return r; }
    static long control(int n) { long r=0; for(int i=0;i<n;i++) r+=(i&3); return r; }
    static long run(int w,int n){ switch(w){
        case 0:return listSize(n); case 1:return listGet(n); case 2:return listSet(n); case 3:return listIter(n);
        case 4:return mapGet(n); case 5:return mapPut(n); case 6:return mapEntry(n);
        case 7:return objEq(n); case 8:return objsEq(n); case 9:return strLen(n); case 10:return strIdx(n);
        case 11:return hash(n); default:return control(n); } }
    static long time(String t,int w,int n,int reps){
        long acc=0; for(int r=0;r<reps;r++) acc+=run(w,n);
        long t0=System.nanoTime(); for(int r=0;r<reps;r++) acc+=run(w,n); long t1=System.nanoTime();
        System.out.println(t+" "+((t1-t0)/((long)n*reps))+" ns/call"); return acc; }
    public static void main(String[] a){
        for(int i=0;i<4;i++){ list.add(new Object()); keys[i]=new Object(); map.put(keys[i], new Object()); }
        int n=100000, reps=12; long acc=0;
        String[] names={"List.size      ","List.get       ","List.set       ","List.iterator  ",
                        "Map.get        ","Map.put        ","Map.entrySet   ","Object.equals  ",
                        "Objects.equals ","String.length  ","String.indexOf ","Object.hashCode","control        "};
        for(int w=0;w<13;w++) acc+=time(names[w],w,n,reps);
        System.out.println("acc="+acc);
    }
}
