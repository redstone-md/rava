import java.util.*;
public class SortStr {
    public static void main(String[] a) {
        String[] xs = {"ints", "LastUpdate", "Status", "DataVersion", "sections", "abc", "ABC", "Zebra", "apple"};
        System.out.println("treeset  " + new TreeSet<>(Arrays.asList(xs)));
        List<String> l = new ArrayList<>(Arrays.asList(xs));
        Collections.sort(l);
        System.out.println("colsort  " + l);
        List<String> l2 = new ArrayList<>(Arrays.asList(xs));
        l2.sort(null);
        System.out.println("listsort " + l2);
        System.out.println("stream   " + l2.stream().sorted().toList());
        System.out.println("cmp L_i  " + "LastUpdate".compareTo("ints"));
        System.out.println("cmp a_A  " + "abc".compareTo("ABC"));
        TreeMap<String,Integer> m = new TreeMap<>();
        for (String x : xs) m.put(x, x.length());
        System.out.println("treemap  " + m.keySet());
        System.out.println("first    " + m.firstKey() + " last " + m.lastKey());
    }
}
