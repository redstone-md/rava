// Reference identity branches compare compressed heap loads with folded references.
public class RefIdentityBranchSem {
    static final Object MARKER = new Object();
    static int total;

    static void hit(int amount) {
        total += amount;
    }

    static void scan(Object[] values) {
        for (int i = 0; i < values.length; i++) {
            if (values[i] == MARKER) hit(1);
            if (MARKER == values[i]) hit(2);
            if (values[i] != MARKER) hit(4);
            if (values[i] == null) hit(8);
        }
    }

    public static void main(String[] args) {
        Object[] values = {MARKER, new Object(), null, MARKER};
        for (int i = 0; i < 100_000; i++) scan(values);
        System.out.println(total);
    }
}
