import java.util.*;
import java.util.function.*;
// Boot-shaped proxy: many small objects, maps keyed by objects, lambdas, interface calls,
// immutable-collection building -- the population MC's registry/block-state bootstrap is made of.
public class BootLike {
    record Prop(String name, List<Integer> values) {}
    static final class State {
        final int block; final Map<Prop,Integer> vals; Map<Prop,State[]> neigh;
        State(int b, Map<Prop,Integer> v) { block = b; vals = v; }
        int hash() { int h = block; for (var e : vals.entrySet()) h += e.getKey().name().hashCode() * 31 + e.getValue(); return h; }
    }
    public static void main(String[] a) {
        long t0 = System.nanoTime();
        List<Prop> props = new ArrayList<>();
        for (int i = 0; i < 40; i++) { List<Integer> v = new ArrayList<>(); for (int j = 0; j < 2 + i % 5; j++) v.add(j); props.add(new Prop("p" + i, List.copyOf(v))); }
        Map<String, List<State>> reg = new LinkedHashMap<>();
        long h = 0; int states = 0;
        for (int b = 0; b < 1100; b++) {
            List<Prop> my = new ArrayList<>(); for (int k = 0; k < 1 + b % 3; k++) my.add(props.get((b * 7 + k * 13) % props.size()));
            List<Map<Prop,Integer>> combos = new ArrayList<>(); combos.add(new HashMap<>());
            for (Prop p : my) { List<Map<Prop,Integer>> nx = new ArrayList<>(); for (var m : combos) for (Integer v : p.values()) { var c = new HashMap<>(m); c.put(p, v); nx.add(c); } combos = nx; }
            final int bb = b;
            List<State> ss = combos.stream().map(m -> new State(bb, Map.copyOf(m))).toList();
            Map<Map<Prop,Integer>, State> idx = new HashMap<>(); for (State s : ss) idx.put(s.vals, s);
            for (State s : ss) { s.neigh = new HashMap<>(); for (Prop p : my) { State[] arr = new State[p.values().size()]; int i = 0; for (Integer v : p.values()) { var m = new HashMap<>(s.vals); m.put(p, v); arr[i++] = idx.get(m); } s.neigh.put(p, arr); } }
            Function<State,Integer> f = State::hash;
            for (State s : ss) h += f.apply(s);
            reg.put("block_" + b, ss); states += ss.size();
        }
        System.out.println("STATES=" + states + " H=" + h + " MS=" + (System.nanoTime() - t0) / 1000000);
    }
}
