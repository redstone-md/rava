import java.util.concurrent.CompletableFuture;
import java.util.concurrent.locks.LockSupport;

public class AotProgressSem {
    static class Payload { int value; Payload(int value) { this.value = value; } }

    public static int work() {
        LockSupport.parkNanos(20000000L);
        int sum = 0;
        for (int i = 0; i < 20000; i++) { sum += new Payload(i).value; }
        return sum == 199990000 ? 42 : -1;
    }

    public static void await(CompletableFuture<Integer> future) {
        while (!future.isDone()) {}
    }

    public static void main(String[] args) {
        Payload kept = new Payload(73);
        CompletableFuture<Integer> future = CompletableFuture.supplyAsync(AotProgressSem::work);
        await(future);
        if (kept.value != 73) { throw new AssertionError("root lost across handoff"); }
        System.out.println(future.join());
    }
}
