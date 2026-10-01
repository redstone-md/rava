import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CompletableFuture;

public class AotWorkerGcSem {
    public static final class Token {
        final int id;
        final Object marker;
        Token(int id, Object marker) { this.id = id; this.marker = marker; }
        public int check(Object expected) {
            for (int i = 0; i < 1; i++) {}
            return marker == expected ? id : -1;
        }
    }
    static long work(Token retained, Object marker) {
        long result = 0;
        for (int i = 0; i < 10000; i++) {
            Token temporary = new Token(i, marker);
            if (temporary.check(marker) != i) throw new AssertionError();
            int value = retained.check(marker);
            if (value < 0) throw new AssertionError();
            result += value;
        }
        return result;
    }
    public static void main(String[] args) {
        Object marker = new Object();
        List<CompletableFuture<Long>> futures = new ArrayList<>();
        for (int i = 0; i < 8; i++) {
            Token retained = new Token(i, marker);
            futures.add(CompletableFuture.supplyAsync(() -> work(retained, marker)));
        }
        long sum = 0;
        for (CompletableFuture<Long> future : futures) sum += future.join();
        System.out.println("sum=" + sum);
    }
}
