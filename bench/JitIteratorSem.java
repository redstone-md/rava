import java.util.*;
public class JitIteratorSem {
    static int after;
    static Object read(Iterator<Integer> it) { Object v = it.next(); after++; return v; }
    public static void main(String[] args) {
        int caught = 0;
        for (int i = 0; i < 4096; i++) {
            List<Integer> list = new ArrayList<>(List.of(1, 2, 3));
            Iterator<Integer> it = list.iterator();
            read(it);
            list.add(4);
            try { read(it); } catch (ConcurrentModificationException e) { caught++; }
        }
        System.out.println("caught=" + caught);
        System.out.println("after=" + after);
    }
}
