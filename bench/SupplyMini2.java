import java.util.concurrent.*;
import java.util.function.*;
public class SupplyMini2 {
    static Integer five() { return 5; }
    public static void main(String[] a) {
        System.out.println("literal   " + CompletableFuture.supplyAsync(() -> 5).join());
        System.out.println("boxed     " + CompletableFuture.supplyAsync(() -> Integer.valueOf(6)).join());
        System.out.println("method    " + CompletableFuture.supplyAsync(SupplyMini2::five).join());
        System.out.println("long      " + CompletableFuture.supplyAsync(() -> 7L).join());
        System.out.println("double    " + CompletableFuture.supplyAsync(() -> 8.5).join());
        System.out.println("bool      " + CompletableFuture.supplyAsync(() -> true).join());
        System.out.println("obj       " + CompletableFuture.supplyAsync(() -> new int[]{1}.length).join());
        Supplier<Integer> s = () -> 9;
        System.out.println("supplier  " + s.get());
        System.out.println("viaVar    " + CompletableFuture.supplyAsync(s).join());
    }
}
