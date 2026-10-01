import java.util.ArrayList;
import java.util.Iterator;

/** Exception edges in callees without catch tables, including later side effects. */
public class AotExceptionSem {
    static final int[] effects = new int[1];
    static void store(Object[] array, Object value) { array[0] = value; effects[0] = effects[0] + 1; }
    static void cast(Object value) { String text = (String) value; effects[0] = effects[0] + 1; }
    static void call(Object value) { value.toString(); effects[0] = effects[0] + 1; }
    static void next(Iterator<?> iterator) { iterator.next(); effects[0] = effects[0] + 1; }
    static void attempt(String label, Runnable action) {
        try { action.run(); System.out.println(label + " ok " + effects[0]); }
        catch (Throwable error) {
            System.out.println(label + " " + error.getClass().getName() + " " + effects[0]);
        }
    }
    public static void main(String[] args) {
        String[] array = new String[1];
        attempt("store-bad", () -> store(array, 1));
        attempt("store-null", () -> store(array, null));
        attempt("store-valid", () -> store(array, "accepted"));
        attempt("cast-bad", () -> cast(1));
        attempt("cast-null", () -> cast(null));
        attempt("cast-valid", () -> cast("accepted"));
        attempt("call-null", () -> call(null));
        attempt("call-valid", () -> call("accepted"));
        Iterator<Object> empty = new ArrayList<>().iterator();
        attempt("next-empty", () -> next(empty));
        attempt("next-empty-again", () -> next(empty));
        ArrayList<Object> values = new ArrayList<>();
        values.add(null);
        Iterator<Object> iterator = values.iterator();
        attempt("next-null-element", () -> next(iterator));
        attempt("next-exhausted", () -> next(iterator));
        System.out.println("stored " + array[0]);
    }
}
