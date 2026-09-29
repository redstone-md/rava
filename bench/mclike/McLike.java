import com.google.common.collect.ImmutableList;
import com.google.common.collect.ImmutableMap;
import com.google.common.collect.ImmutableSortedMap;
import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import io.netty.bootstrap.ServerBootstrap;
import io.netty.channel.Channel;
import io.netty.channel.ChannelInitializer;
import io.netty.channel.EventLoopGroup;
import io.netty.channel.nio.NioEventLoopGroup;
import io.netty.channel.socket.SocketChannel;
import io.netty.channel.socket.nio.NioServerSocketChannel;
import it.unimi.dsi.fastutil.objects.Object2IntOpenHashMap;
import it.unimi.dsi.fastutil.objects.ObjectArrayList;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.function.Function;
import java.util.stream.Collectors;
import joptsimple.OptionParser;
import joptsimple.OptionSet;
import org.apache.logging.log4j.LogManager;
import org.apache.logging.log4j.Logger;

/**
 * A Minecraft-server-shaped startup built only from the server's open-source dependencies
 * (log4j2, gson, guava, fastutil, netty, jopt-simple). Phases mirror the real boot: option
 * parsing, logging, registry/block-state bootstrap, data-driven JSON decoded through codec-like
 * combinators, noise-based terrain sampling, and binding a netty server. Every phase folds its
 * result into a hash so engines can be compared for agreement as well as speed.
 *
 * Usage: java -cp out:lib/* McLike [--chunks N]
 */
public final class McLike {
    private static final Logger LOGGER = LogManager.getLogger("McLike");

    record Property(String name, List<String> values) {}

    record BlockState(int block, ImmutableMap<Property, String> values) {
        int fold() {
            int h = block;
            for (Map.Entry<Property, String> e : values.entrySet()) h += e.getKey().name().hashCode() * 31 ^ e.getValue().hashCode();
            return h;
        }
    }

    record Block(String id, List<BlockState> states) {}

    record Ingredient(String item, int count) {}

    record Recipe(String id, String type, List<Ingredient> ingredients, Optional<String> group, float experience) {}

    /** A tiny codec: decode JSON into a value or fail with a message. */
    interface Codec<T> {
        T decode(JsonElement e);

        default <R> Codec<R> map(Function<? super T, ? extends R> f) {
            return e -> f.apply(decode(e));
        }

        static Codec<String> string() {
            return JsonElement::getAsString;
        }

        static Codec<Integer> integer() {
            return JsonElement::getAsInt;
        }

        default Codec<List<T>> list() {
            return e -> {
                JsonArray a = e.getAsJsonArray();
                List<T> out = new ArrayList<>(a.size());
                for (JsonElement x : a) out.add(decode(x));
                return ImmutableList.copyOf(out);
            };
        }

        default Function<JsonObject, Optional<T>> optionalField(String name) {
            return o -> o.has(name) ? Optional.of(decode(o.get(name))) : Optional.empty();
        }
    }

    static final Codec<Ingredient> INGREDIENT = e -> {
        JsonObject o = e.getAsJsonObject();
        return new Ingredient(Codec.string().decode(o.get("item")), o.has("count") ? Codec.integer().decode(o.get("count")) : 1);
    };

    static final Codec<Recipe> RECIPE = e -> {
        JsonObject o = e.getAsJsonObject();
        return new Recipe(
                o.get("id").getAsString(),
                o.get("type").getAsString(),
                INGREDIENT.list().decode(o.get("ingredients")),
                Codec.string().optionalField("group").apply(o),
                o.has("experience") ? o.get("experience").getAsFloat() : 0f);
    };

    public static void main(String[] args) throws Exception {
        long start = System.nanoTime();
        OptionParser parser = new OptionParser();
        var chunksOpt = parser.accepts("chunks").withRequiredArg().ofType(Integer.class).defaultsTo(9);
        OptionSet options = parser.parse(args);
        int chunks = options.valueOf(chunksOpt);
        LOGGER.info("Starting McLike server");

        long t0 = System.nanoTime();
        List<Block> blocks = bootstrapRegistry();
        long hBlocks = 0;
        for (Block b : blocks) for (BlockState s : b.states()) hBlocks = hBlocks * 31 + s.fold();
        long t1 = System.nanoTime();
        LOGGER.info("Bootstrapped {} blocks with {} states in {} ms", blocks.size(), blocks.stream().mapToInt(b -> b.states().size()).sum(), (t1 - t0) / 1_000_000);

        List<Recipe> recipes = loadDatapack(4000);
        long hRecipes = recipes.stream().mapToLong(r -> r.id().hashCode() * 17L + r.ingredients().size() + r.group().map(String::length).orElse(0)).sum();
        long t2 = System.nanoTime();
        LOGGER.info("Loaded {} recipes in {} ms", recipes.size(), (t2 - t1) / 1_000_000);

        long hTerrain = generateTerrain(chunks);
        long t3 = System.nanoTime();
        LOGGER.info("Preparing spawn area: {} chunks in {} ms", chunks, (t3 - t2) / 1_000_000);

        int port = bindServer();
        long t4 = System.nanoTime();
        LOGGER.info("Bound netty server on port {} in {} ms", port > 0 ? "*" : "-", (t4 - t3) / 1_000_000);

        LOGGER.info("Done ({}s)! HASH={}/{}/{}", String.format("%.3f", (t4 - start) / 1e9), Long.toHexString(hBlocks), Long.toHexString(hRecipes), Long.toHexString(hTerrain));
        System.out.println("MCLIKE_MS=" + (t4 - start) / 1_000_000 + " HASH=" + Long.toHexString(hBlocks ^ hRecipes ^ hTerrain));
    }

    static List<Block> bootstrapRegistry() {
        List<Property> props = new ArrayList<>();
        String[][] domains = {{"true", "false"}, {"north", "south", "east", "west"}, {"0", "1", "2", "3", "4", "5", "6", "7"}, {"top", "bottom"}, {"x", "y", "z"}};
        for (int i = 0; i < 48; i++) props.add(new Property("p" + i, ImmutableList.copyOf(domains[i % domains.length])));
        Object2IntOpenHashMap<String> ids = new Object2IntOpenHashMap<>();
        ObjectArrayList<Block> blocks = new ObjectArrayList<>();
        for (int b = 0; b < 1100; b++) {
            List<Property> mine = new ArrayList<>();
            for (int k = 0; k < 1 + b % 3; k++) {
                Property p = props.get((b * 7 + k * 13) % props.size());
                if (!mine.contains(p)) mine.add(p);
            }
            List<ImmutableMap<Property, String>> combos = new ArrayList<>();
            combos.add(ImmutableMap.of());
            for (Property p : mine) {
                List<ImmutableMap<Property, String>> next = new ArrayList<>();
                for (ImmutableMap<Property, String> m : combos) {
                    for (String v : p.values()) {
                        next.add(ImmutableMap.<Property, String>builder().putAll(m).put(p, v).build());
                    }
                }
                combos = next;
            }
            final int id = b;
            List<BlockState> states = combos.stream().map(m -> new BlockState(id, m)).collect(Collectors.toUnmodifiableList());
            String name = "minecraft:block_" + b;
            ids.put(name, b);
            blocks.add(new Block(name, states));
        }
        ImmutableSortedMap<String, Integer> sorted = ImmutableSortedMap.copyOf(ids);
        if (sorted.size() != blocks.size()) throw new IllegalStateException();
        return blocks;
    }

    static List<Recipe> loadDatapack(int count) {
        Gson gson = new GsonBuilder().setPrettyPrinting().create();
        List<Recipe> out = new ArrayList<>(count);
        for (int i = 0; i < count; i++) {
            JsonObject o = new JsonObject();
            o.addProperty("id", "minecraft:recipe_" + i);
            o.addProperty("type", i % 3 == 0 ? "crafting_shaped" : "crafting_shapeless");
            if (i % 2 == 0) o.addProperty("group", "g" + (i % 17));
            if (i % 5 == 0) o.addProperty("experience", 0.1f * (i % 7));
            JsonArray ing = new JsonArray();
            for (int k = 0; k < 1 + i % 6; k++) {
                JsonObject in = new JsonObject();
                in.addProperty("item", "minecraft:item_" + ((i * 31 + k) % 900));
                if (k % 2 == 1) in.addProperty("count", k);
                ing.add(in);
            }
            o.add("ingredients", ing);
            String text = gson.toJson(o);
            out.add(RECIPE.decode(JsonParser.parseString(text)));
        }
        return out;
    }

    /** Ken Perlin's improved noise, octave-summed like a terrain density function. */
    static final class ImprovedNoise {
        private final byte[] p = new byte[512];

        ImprovedNoise(long seed) {
            for (int i = 0; i < 256; i++) p[i] = (byte) i;
            long s = seed;
            for (int i = 255; i > 0; i--) {
                s = s * 6364136223846793005L + 1442695040888963407L;
                int j = (int) ((s >>> 33) % (i + 1));
                byte t = p[i];
                p[i] = p[j];
                p[j] = t;
            }
            System.arraycopy(p, 0, p, 256, 256);
        }

        private static double fade(double t) {
            return t * t * t * (t * (t * 6 - 15) + 10);
        }

        private static double lerp(double t, double a, double b) {
            return a + t * (b - a);
        }

        private static double grad(int hash, double x, double y, double z) {
            int h = hash & 15;
            double u = h < 8 ? x : y, v = h < 4 ? y : h == 12 || h == 14 ? x : z;
            return ((h & 1) == 0 ? u : -u) + ((h & 2) == 0 ? v : -v);
        }

        private int p(int i) {
            return p[i & 511] & 255;
        }

        double noise(double x, double y, double z) {
            int X = (int) Math.floor(x) & 255, Y = (int) Math.floor(y) & 255, Z = (int) Math.floor(z) & 255;
            x -= Math.floor(x);
            y -= Math.floor(y);
            z -= Math.floor(z);
            double u = fade(x), v = fade(y), w = fade(z);
            int A = p(X) + Y, AA = p(A) + Z, AB = p(A + 1) + Z, B = p(X + 1) + Y, BA = p(B) + Z, BB = p(B + 1) + Z;
            return lerp(w, lerp(v, lerp(u, grad(p(AA), x, y, z), grad(p(BA), x - 1, y, z)),
                            lerp(u, grad(p(AB), x, y - 1, z), grad(p(BB), x - 1, y - 1, z))),
                    lerp(v, lerp(u, grad(p(AA + 1), x, y, z - 1), grad(p(BA + 1), x - 1, y, z - 1)),
                            lerp(u, grad(p(AB + 1), x, y - 1, z - 1), grad(p(BB + 1), x - 1, y - 1, z - 1))));
        }
    }

    interface DensityFunction {
        double compute(int x, int y, int z);
    }

    static long generateTerrain(int chunks) {
        ImprovedNoise[] octaves = new ImprovedNoise[6];
        for (int i = 0; i < octaves.length; i++) octaves[i] = new ImprovedNoise(1234L + i);
        DensityFunction base = (x, y, z) -> {
            double v = 0, f = 1.0 / 128, a = 1;
            for (ImprovedNoise n : octaves) {
                v += n.noise(x * f, y * f, z * f) * a;
                f *= 2;
                a *= 0.5;
            }
            return v;
        };
        DensityFunction density = (x, y, z) -> base.compute(x, y, z) - (y - 64) / 64.0;
        long h = 0;
        int side = (int) Math.ceil(Math.sqrt(chunks));
        for (int c = 0; c < chunks; c++) {
            int cx = (c % side) * 16, cz = (c / side) * 16;
            for (int x = 0; x < 16; x += 2)
                for (int z = 0; z < 16; z += 2)
                    for (int y = -64; y < 320; y += 4)
                        if (density.compute(cx + x, y, cz + z) > 0) h = h * 31 + (x ^ (y << 4) ^ (z << 12));
        }
        return h;
    }

    static int bindServer() throws InterruptedException {
        EventLoopGroup boss = new NioEventLoopGroup(1);
        try {
            Channel ch = new ServerBootstrap()
                    .group(boss)
                    .channel(NioServerSocketChannel.class)
                    .childHandler(new ChannelInitializer<SocketChannel>() {
                        @Override
                        protected void initChannel(SocketChannel c) {}
                    })
                    .bind(0).sync().channel();
            int port = ((java.net.InetSocketAddress) ch.localAddress()).getPort();
            ch.close().sync();
            return port;
        } finally {
            boss.shutdownGracefully(0, 0, java.util.concurrent.TimeUnit.MILLISECONDS).sync();
        }
    }
}
