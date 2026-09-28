// Exercise collection contracts after lambda bodies become compiled.
import java.util.*;
public class JitListSem {
    static int refused(Runnable action) {
        int count = 0;
        for (int i = 0; i < 4096; i++) {
            try { action.run(); }
            catch (UnsupportedOperationException expected) { count++; }
        }
        return count;
    }
    static void removeLast(List<Integer> list) { list.remove(list.size() - 1); }
    public static void main(String[] args) {
        List<Integer> ro = Collections.unmodifiableList(new ArrayList<>(List.of(1)));
        System.out.println("add=" + refused(() -> ro.add(2)) + " size=" + ro.size());
        System.out.println("set=" + refused(() -> ro.set(0, 2)) + " first=" + ro.get(0));
        System.out.println("remove=" + refused(() -> ro.remove(0)) + " size=" + ro.size());
        List<Integer> fixed = Arrays.asList(1, 2);
        System.out.println("fixed-add=" + refused(() -> fixed.add(3)));
        System.out.println("fixed-set=" + fixed.set(0, 7) + ":" + fixed.size()
            + ":" + fixed.get(0) + ":" + fixed.get(1));
        List<Integer> mutable = new ArrayList<>(List.of(1));
        Runnable append = () -> mutable.add(2);
        int cme = 0;
        for (int i = 0; i < 4096; i++) {
            Iterator<Integer> iterator = mutable.iterator();
            append.run();
            try { iterator.next(); }
            catch (ConcurrentModificationException expected) { cme++; }
            removeLast(mutable);
        }
        System.out.println("cme=" + cme + " size=" + mutable.size());
    }
}
