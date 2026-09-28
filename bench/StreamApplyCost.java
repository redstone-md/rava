// What does ONE guest lambda application through the native stream layer cost?
//
// `bridgestat` puts `Stream#map` at 2.92 s of self-time over 131,324 calls on an MC boot, and
// `RAVA_STREAM_MAP_STAT` puts the average pipeline at 3.9 elements — so ~5.7 us per application.
// That number INCLUDES the lambda body, and the split between engine overhead and useful work
// was never measured. An identity body makes everything that remains overhead.
import java.util.ArrayList;
import java.util.List;

public class StreamApplyCost {
    static long sink;

    // ⚠**THE MEASURED BODY MUST BE ITS OWN METHOD.** With the loop inline in `viaX`, the whole
    // benchmark is ONE guest invocation — `RAVA_DIAG=exec` read `compiled=6 interpreted=9` — so it
    // never crosses the tiering threshold and measures the INTERPRETED native path. The boot's
    // natives are reached from compiled code, where `fu_fastpath` applies at all. One step per
    // call makes the step compile and the probe measure the same population the boot does.
    static long stepStream(List<Integer> src) {
        long acc = 0;
        for (Integer v : src.stream().map(x -> x).toList()) acc += v;
        return acc;
    }

    static long stepLoop(List<Integer> src) {
        long acc = 0;
        List<Integer> out = new ArrayList<>();
        for (Integer v : src) out.add(v);
        for (Integer v : out) acc += v;
        return acc;
    }

    static long viaStream(List<Integer> src, int reps) {
        long acc = 0;
        for (int r = 0; r < reps; r++) acc += stepStream(src);
        return acc;
    }

    static long viaLoop(List<Integer> src, int reps) {
        long acc = 0;
        for (int r = 0; r < reps; r++) acc += stepLoop(src);
        return acc;
    }

    public static void main(String[] a) {
        // 4 elements: the measured average pipeline width on a real boot.
        List<Integer> src = new ArrayList<>();
        for (int i = 0; i < 4; i++) src.add(i);
        int reps = 200_000;
        for (int w = 0; w < 3; w++) { sink += viaStream(src, 20_000); sink += viaLoop(src, 20_000); }

        long t0 = System.nanoTime();
        sink += viaStream(src, reps);
        long streamNs = System.nanoTime() - t0;

        t0 = System.nanoTime();
        sink += viaLoop(src, reps);
        long loopNs = System.nanoTime() - t0;

        long apps = (long) reps * src.size();
        System.out.println("STREAM_NS_PER_APPLY=" + (streamNs / apps));
        System.out.println("LOOP_NS_PER_ELEM=" + (loopNs / apps));
        System.out.println("SINK=" + sink);
    }
}
