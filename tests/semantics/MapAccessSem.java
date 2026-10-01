// Differential coverage for access order through Map and HashMap call sites.
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.Map;

public class MapAccessSem {
    static Object get(HashMap<String, String> map, String key) {
        return map.get(key);
    }

    static Object fallback(HashMap<String, String> map, String key) {
        return map.getOrDefault(key, "fallback");
    }

    static void print(String label, Object value, Map<String, String> map) {
        System.out.println(label + "=" + value + ":" + map.keySet());
    }

    public static void main(String[] args) {
        HashMap<String, String> warm = new LinkedHashMap<>(16, 0.75f, true);
        warm.put("a", "A");
        warm.put("b", null);
        warm.put("c", "C");
        for (int i = 0; i < 20000; i++) {
            get(warm, "a");
            get(warm, "c");
            fallback(warm, "b");
        }
        print("warm", get(warm, "a"), warm);
        print("null", fallback(warm, "b"), warm);
        print("missing", fallback(warm, "missing"), warm);
        print("tail", get(warm, "b"), warm);
        print("contains", warm.containsKey("c"), warm);

        Map<String, String> generic = new LinkedHashMap<>(16, 0.75f, true);
        generic.put("a", "A");
        generic.put("b", null);
        generic.put("c", "C");
        print("generic.get", generic.get("a"), generic);
        print("generic.null", generic.getOrDefault("b", "fallback"), generic);
        print("generic.missing", generic.get("missing"), generic);

        HashMap<String, String> insertion = new LinkedHashMap<>();
        insertion.put("a", "A");
        insertion.put("b", null);
        insertion.put("c", "C");
        print("insertion", get(insertion, "a"), insertion);
        print("insertion.null", fallback(insertion, "b"), insertion);

        String stored = new String("same");
        String lookup = new String("same");
        Map<String, String> identity = new LinkedHashMap<>(16, 0.75f, true);
        identity.put(stored, "value");
        identity.get(lookup);
        System.out.println("canonical=" + (identity.keySet().iterator().next() == stored));
    }
}
