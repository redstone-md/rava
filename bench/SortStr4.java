import java.util.*;
public class SortStr4 {
    public static void main(String[] a) {
        TreeSet<String> t = new TreeSet<>();
        for (String k : new String[]{"ints","LastUpdate","Status","DataVersion"}) t.add(k);
        System.out.println("tostring " + t);
        System.out.println("arraylist " + new ArrayList<>(t));
        StringBuilder b = new StringBuilder();
        Iterator<String> it = t.iterator();
        while (it.hasNext()) b.append(it.next()).append(' ');
        System.out.println("manual   " + b.toString().trim());
        StringBuilder c = new StringBuilder();
        for (String x : t) c.append(x).append(' ');
        System.out.println("foreach  " + c.toString().trim());
        System.out.println("stream   " + t.stream().toList());
        System.out.println("toArray  " + Arrays.toString(t.toArray()));
    }
}
