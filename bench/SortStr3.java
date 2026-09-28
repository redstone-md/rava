import java.util.*;
public class SortStr3 {
    public static void main(String[] a) {
        Map<String,Integer> m = new HashMap<>();
        m.put("ints",1); m.put("LastUpdate",2); m.put("Status",3); m.put("DataVersion",4);
        Set<String> ks = m.keySet();
        System.out.println("ts-from-keyset  " + new TreeSet<>(ks));
        System.out.println("ts-from-copy    " + new TreeSet<>(new ArrayList<>(ks)));
        System.out.println("ts-addAll       " + addAll(ks));
        TreeSet<String> t = new TreeSet<>();
        for (String k : ks) t.add(k);
        System.out.println("ts-loop-add     " + t);
        System.out.println("ts-first/last   " + t.first() + " " + t.last());
        System.out.println("ts-iter         " + iter(t));
    }
    static TreeSet<String> addAll(Collection<String> c) { TreeSet<String> t = new TreeSet<>(); t.addAll(c); return t; }
    static String iter(Set<String> s) { StringBuilder b = new StringBuilder(); for (String x : s) b.append(x).append(' '); return b.toString().trim(); }
}
