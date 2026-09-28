import java.util.*;
import java.util.stream.*;

// Fusion correctness + micro-benchmark. Output MUST be identical eager vs lazy
// (RAVA_LAZY_STREAMS). Exercises deferred ops (map/mapToX/filter/peek) + terminals
// + barriers (sorted/distinct/flatMap/limit/skip) so a lazy bug surfaces as a
// checksum mismatch.
public class StreamFuse {
    public static void main(String[] a) {
        int N = a.length > 0 ? Integer.parseInt(a[0]) : 200_000;
        long t0 = System.nanoTime();
        long acc = 0;

        // 1) map -> filter -> map -> sum  (pure fused chain)
        for (int r0 = 0; r0 < 20; r0++) { final int r = r0;
            long s = IntStream.range(0, N)
                    .map(x -> x + r)
                    .filter(x -> (x & 1) == 0)
                    .map(x -> x * 3)
                    .asLongStream()
                    .sum();
            acc += s;
        }

        System.out.println("S1="+acc);
        // 2) boxed map/filter -> collect(toList) -> size
        List<Integer> base = new ArrayList<>();
        for (int i = 0; i < 5000; i++) base.add(i);
        for (int r0 = 0; r0 < 20; r0++) { final int r = r0;
            List<Integer> ys = base.stream()
                    .map(x -> x + r)
                    .filter(x -> x % 3 == 0)
                    .map(x -> x * 2)
                    .collect(Collectors.toList());
            acc += ys.size();
            acc += ys.get(ys.size() - 1);
        }

        System.out.println("S2="+acc);
        // 3) peek side-effect + reduce
        int[] seen = {0};
        for (int r0 = 0; r0 < 20; r0++) { final int r = r0;
            Optional<Integer> red = base.stream()
                    .map(x -> x + 1)
                    .peek(x -> seen[0]++)
                    .filter(x -> x > 100)
                    .reduce((x, y) -> x + y);
            acc += red.orElse(0);
        }
        acc += seen[0];

        System.out.println("S3="+acc);
        // 4) barrier ops: sorted, distinct, limit, skip, flatMap
        for (int r0 = 0; r0 < 10; r0++) { final int r = r0;
            long s = base.stream()
                    .map(x -> (x * 7 + r) % 500)
                    .distinct()
                    .sorted()
                    .skip(5)
                    .limit(50)
                    .mapToInt(Integer::intValue)
                    .sum();
            acc += s;
            List<Integer> fm = base.stream()
                    .filter(x -> x < 100)
                    .flatMap(x -> Stream.of(x, x + 1000))
                    .map(x -> x + r)
                    .collect(Collectors.toList());
            acc += fm.size();
        }

        System.out.println("S4="+acc);
        // 5) count + forEach
        for (int r0 = 0; r0 < 20; r0++) { final int r = r0;
            long c = base.stream().map(x -> x + r).filter(x -> x % 7 == 0).count();
            acc += c;
        }
        int[] fsum = {0};
        base.stream().map(x -> x * 2).filter(x -> x % 4 == 0).forEach(x -> fsum[0] += x);
        acc += fsum[0];

        System.out.println("S5="+acc);
        System.out.println("SEC_TOTAL=" + acc);
        long ms = (System.nanoTime() - t0) / 1_000_000;
        System.out.println("StreamFuse_CHECKSUM=" + acc);
        System.out.println("StreamFuse_MS=" + ms);
    }
}
