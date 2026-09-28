import java.util.*;
public class SortStr2 {
    public static void main(String[] a) {
        LinkedHashSet<String> src = new LinkedHashSet<>(Arrays.asList("ints","LastUpdate","Status","DataVersion"));
        System.out.println("from-set   " + new TreeSet<>(src));
        System.out.println("from-list  " + new TreeSet<>(new ArrayList<>(src)));
        TreeMap<String,Integer> m = new TreeMap<>();
        for (String x : src) m.put(x, 1);
        System.out.println("map.keySet " + m.keySet());
        System.out.println("map.values " + m.values());
        System.out.println("map.entry  " + m.entrySet());
        System.out.println("map.tostr  " + m);
        System.out.println("map.navkey " + m.navigableKeySet());
        System.out.println("map.desc   " + m.descendingKeySet());
        System.out.println("map.head   " + m.headMap("S"));
        System.out.println("foreach:");
        for (String k : m.keySet()) System.out.println("  " + k);
    }
}
