// REMOVAL AND VIEW-MUTATION under RAVA_ARENA_MAPS. RegistrySetBuilder reports
// "Errors during registry creation" for values that were registered and then never
// claimed — i.e. for entries a removal failed to take out.
import java.util.*;
public class AmRem {
    record K(String p) {}
    public static void main(String[] a){
        Map<K,String> m = new HashMap<>();
        for (int i=0;i<20;i++) m.put(new K("k"+i), "v"+i);
        int removed=0; for (int i=0;i<20;i++) if (m.remove(new K("k"+i))!=null) removed++;
        System.out.println("remove(objkey) removed=" + removed + "/20 left=" + m.size());

        Map<K,String> m2 = new HashMap<>();
        for (int i=0;i<20;i++) m2.put(new K("q"+i), "v"+i);
        Iterator<Map.Entry<K,String>> it = m2.entrySet().iterator();
        int viaIter=0; while (it.hasNext()) { it.next(); it.remove(); viaIter++; }
        System.out.println("entrySet.iterator.remove n=" + viaIter + " left=" + m2.size());

        Map<K,String> m3 = new HashMap<>();
        for (int i=0;i<20;i++) m3.put(new K("r"+i), "v"+i);
        boolean ks = m3.keySet().remove(new K("r0"));
        boolean vs = m3.values().remove("v1");
        m3.entrySet().removeIf(e -> e.getValue().equals("v2"));
        System.out.println("keySet.remove=" + ks + " values.remove=" + vs + " removeIf left=" + m3.size());

        Map<K,String> m4 = new HashMap<>();
        for (int i=0;i<20;i++) m4.put(new K("s"+i), "v"+i);
        m4.keySet().retainAll(Set.of(new K("s0"), new K("s1")));
        System.out.println("keySet.retainAll left=" + m4.size() + " keys=" + m4.keySet().size());

        Map<K,String> m5 = new HashMap<>();
        m5.put(new K("t"), "1");
        System.out.println("putIfAbsent=" + m5.putIfAbsent(new K("t"),"2")
            + " computeIfAbsent=" + m5.computeIfAbsent(new K("u"), k->"3")
            + " merge=" + m5.merge(new K("t"), "z", (p,q)->p+q)
            + " replace=" + m5.replace(new K("t"), "9")
            + " final=" + m5.size());
    }
}
