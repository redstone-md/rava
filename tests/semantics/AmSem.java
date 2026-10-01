// ARENA-MAP SEMANTICS PROBE. Under RAVA_ARENA_MAPS a java.util.HashMap becomes an arena
// object; every line here is a contract the registry build relies on.
import java.util.*;
import java.util.stream.*;
public class AmSem {
    public static void main(String[] a){
        Map<String,String> m = new HashMap<>();
        System.out.println("empty.isEmpty=" + m.isEmpty() + " size=" + m.size()
            + " entrySet.size=" + m.entrySet().size() + " es.isEmpty=" + m.entrySet().isEmpty()
            + " stream.count=" + m.entrySet().stream().count());
        m.put("k","v");
        System.out.println("one.isEmpty=" + m.isEmpty() + " size=" + m.size()
            + " get=" + m.get("k") + " entrySet.size=" + m.entrySet().size()
            + " stream.count=" + m.entrySet().stream().count()
            + " joined=" + m.entrySet().stream().map(e->e.getKey()+"="+e.getValue()).collect(Collectors.joining(",")));
        m.remove("k");
        System.out.println("gone.isEmpty=" + m.isEmpty() + " size=" + m.size()
            + " entrySet.size=" + m.entrySet().size() + " stream.count=" + m.entrySet().stream().count());
        Map<String,String> im = Map.of("a","1","b","2");
        System.out.println("Map.of size=" + im.size() + " isEmpty=" + im.isEmpty()
            + " keys=" + new TreeSet<>(im.keySet()));
        try { im.put("c","3"); System.out.println("IMMUTABLE-PUT-ALLOWED (WRONG)"); }
        catch (UnsupportedOperationException e) { System.out.println("Map.of put -> UOE (right)"); }
        Map<String,String> lhm = new LinkedHashMap<>();
        lhm.put("x","1"); lhm.put("y","2");
        System.out.println("lhm=" + lhm.size() + " " + lhm.keySet() + " values=" + lhm.values());
    }
}
