import com.google.gson.*;
import java.io.StringReader;
public class GsonRead {
    public static void main(String[] a) {
        String json = "{\"a\":1,\"b\":[true,\"y\",-2],\"c\":null}";
        JsonElement e = JsonParser.parseReader(new StringReader(json));
        JsonObject o = e.getAsJsonObject();
        System.out.println("SUM=" + (o.get("a").getAsInt() + o.getAsJsonArray("b").get(2).getAsInt()));
        System.out.println("RT=" + o.toString());
    }
}
