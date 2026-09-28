// TreeMap's contract IS its order. `firstKey` returning whichever key was inserted first is a
// silent wrong answer, not a missing feature.
import java.util.TreeMap;

public class TreeOrd {
    public static void main(String[] a) {
        TreeMap<String, Integer> m = new TreeMap<>();
        m.put("pear", 3); m.put("apple", 1); m.put("fig", 2);
        System.out.println("firstKey=" + m.firstKey());
        System.out.println("lastKey=" + m.lastKey());
        TreeMap<Integer, String> n = new TreeMap<>();
        n.put(30, "c"); n.put(10, "a"); n.put(20, "b");
        System.out.println("firstKeyI=" + n.firstKey());
        System.out.println("lastKeyI=" + n.lastKey());
        System.out.println("firstEntry=" + m.firstEntry());
        System.out.println("lastEntry=" + m.lastEntry());
    }
}
