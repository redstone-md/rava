// Heterogeneous completed tasks must not be replayed based on callback counts.
// RAVA_CALLBACK_STATS=1 exposes the old pilot-count-dependent routing defect.
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletableFuture;

public class LaneWorkSem {
    static Object work(Map<Integer, Object> map, int iterations, Object token) {
        Object found = token;
        for (int i = 0; i < iterations; i++) found = map.get(1);
        return found;
    }

    public static void main(String[] args) {
        Object token = new Object();
        Map<Integer, Object> map = new HashMap<>();
        map.put(1, token);
        for (int i = 0; i < 1000; i++) {
            if (work(map, 1, token) != token) throw new AssertionError();
        }
        List<CompletableFuture<Object>> futures = new ArrayList<>();
        for (int i = 0; i < 8; i++) {
            int iterations = i == 0 ? 100000 : i;
            futures.add(CompletableFuture.supplyAsync(() -> work(map, iterations, token)));
        }
        int matches = 0;
        for (CompletableFuture<Object> future : futures) {
            if (future.join() == token) matches++;
        }
        System.out.println("identical=" + matches);
    }
}
