// Retained reference results across sibling/nested inlines and moving GC.
public class InlinePoolSem {
    static final class Cell {
        final int value;
        Cell(int value) { this.value = value; }
    }
    static final class Holder {
        final Cell first, second;
        Holder(int n) { first = new Cell(n); second = new Cell(~n); }
    }
    static Cell choose(Holder h, boolean first) { return first ? h.first : h.second; }
    static Cell nested(Holder h) { return choose(h, true); }
    static int churn(int n) {
        Object[] values = new Object[1200];
        for (int i = 0; i < values.length; i++) values[i] = new Cell(n + i);
        return ((Cell) values[values.length - 1]).value;
    }
    static int check(Cell first, Cell second, int noise, int n) {
        if (first.value != n || second.value != ~n || noise != n + 1199)
            throw new AssertionError("stale inline result");
        return first.value ^ second.value;
    }
    static int exercise(Holder h, int n) {
        return check(nested(h), choose(h, false), churn(n), n);
    }
    public static void main(String[] args) {
        int total = 0;
        for (int n = 0; n < 200; n++) total += exercise(new Holder(n), n);
        System.out.println("inline-pool " + total);
    }
}
