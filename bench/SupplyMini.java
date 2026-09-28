// Differential probe: CompletableFuture.supplyAsync, the API the gil-free lane is
// entered through. It caught the lane publishing a thread-local box index across
// threads (ledger row 278), which made every boxed numeric result 0.
//
// ONE LINE IS EXPECTED TO DIVERGE:
//
//   done   the JVM prints `false 11` — supplyAsync is genuinely asynchronous there, so
//          isDone() is false right after the call. This engine runs the supplier inline
//          or defers it to a lane that has already drained by the time isDone() is asked,
//          so it prints `true 11`. The VALUE is the same; only the observation of
//          in-flight-ness differs, and CompletableFuture's contract permits a future
//          that is already complete.
import java.util.concurrent.*;
public class SupplyMini {
    public static void main(String[] a) throws Exception {
        System.out.println("direct " + CompletableFuture.supplyAsync(() -> 5).join());
        System.out.println("get    " + CompletableFuture.supplyAsync(() -> 7).get());
        System.out.println("str    " + CompletableFuture.supplyAsync(() -> "s").join());
        System.out.println("chain  " + CompletableFuture.supplyAsync(() -> 3).thenApply(x -> x + 1).join());
        CompletableFuture<Integer> cf = CompletableFuture.supplyAsync(() -> 11);
        System.out.println("done   " + cf.isDone() + " " + cf.join());
        System.out.println("exec   " + CompletableFuture.supplyAsync(() -> 13, ForkJoinPool.commonPool()).join());
    }
}
