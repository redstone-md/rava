// A reducing sink reached through merges: IntPipeline.sum's compiled code once returned the
// sink's initial state for most calls, as jump threading took the field of the fresh sink to
// hold its one store when stores reached it through a phi (the stages' wrapped sinks).

import java.util.ArrayList;
import java.util.List;

public class StreamSinkMergeSem {
    public static void main(String[] args) {
        List<Integer> ints = new ArrayList<>();
        for (int i = 0; i < 1000; i++) ints.add(i);
        long sink = 0;
        for (int i = 0; i < 40000; i++) sink += ints.stream().filter(x -> (x & 1) == 0).mapToInt(x -> x * 3).sum();
        System.out.println("sum " + sink);
        long count = 0;
        for (int i = 0; i < 20000; i++) count += ints.stream().filter(x -> x % 3 == 0).count();
        System.out.println("count " + count);
    }
}
