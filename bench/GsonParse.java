import com.google.gson.*;

// Exercises native parseString → tree read-back across every JsonElement kind:
// nested object/array, int/double/negative numbers, string with escape, bool, null.
public class GsonParse {
    public static void main(String[] args) {
        String json = "{\"a\":1,\"b\":[true,\"x\\n\",2,-3],\"c\":null,\"d\":{\"n\":-42,\"f\":3.5}}";
        JsonElement e = JsonParser.parseString(json);
        JsonObject o = e.getAsJsonObject();
        long sum = 0;
        sum += o.get("a").getAsInt();                         // 1
        sum += o.getAsJsonArray("b").get(2).getAsInt();       // 2
        sum += o.getAsJsonArray("b").get(3).getAsInt();       // -3
        sum += o.getAsJsonObject("d").get("n").getAsInt();    // -42
        sum += (long) (o.getAsJsonObject("d").get("f").getAsDouble() * 10); // 35
        sum += o.getAsJsonArray("b").get(0).getAsBoolean() ? 1000 : 0;      // 1000
        System.out.println("SUM=" + sum);
        System.out.println("CNULL=" + o.get("c").isJsonNull());
        System.out.println("BSTR=" + o.getAsJsonArray("b").get(1).getAsString());
        System.out.println("HASD=" + o.has("d"));
        System.out.println("KEYS=" + o.keySet());
        System.out.println("RT=" + o.toString());
    }
}
