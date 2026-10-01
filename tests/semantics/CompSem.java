// Differential probe: DATA COMPONENT PATCHES — the identity-keyed fastutil map behind every
// ItemStack's components, built, copied, iterated and round-tripped through its codec.
//
// WHY THIS AREA. bench/ReloadBench.java folds every recipe through Recipe.CODEC after the
// datapack reload and died under rava with
//   ClassCastException: class [arena-string] cannot be cast to class DataComponentType
// while ENCODING a recipe result's DataComponentPatch: iterating the patch's
// `Reference2ObjectMap<DataComponentType, Optional<?>>` produced a KEY that was a string. The
// keys enter that map from `PatchKey.type()` (a registry object) or from `Builder.set`, so a
// string key can only be manufactured by the engine — a native map arm that stores a key's
// render instead of the key, an entry view that hands back the wrong cell, or a copy that
// materialises keys wrongly. This probe walks each of those doors with the real Minecraft
// classes and prints the CLASS of every key it sees, so the divergence is visible at the first
// map operation that produces it rather than 44 s into a reload.
//
// Needs the Minecraft jar (bench\sem_all.ps1 puts it on the classpath); Bootstrap.bootStrap()
// runs once and replaces System.out, so the real stream is captured first. Every line is a
// deterministic observable.
//
// THREE LINES ARE EXPECTED TO DIVERGE, and none of them is an observable this probe prints:
// they are Log4j's own StatusLogger, on stdout, and they exist ONLY on the rava side.
//
//   [..] [ServerMain/ERROR]: Log4j API could not find a logging provider.
//   [..] [ServerMain/INFO]: Using provider:
//   Provider 'org.apache.logging.log4j.simple.internal.SimpleProvider'
//
// ⛔**THE CAUSE IS `ServiceLoader`, AND IT IS MEASURED.** `log4j-core-2.25.2.jar` ships
// `META-INF/services/org.apache.logging.log4j.spi.Provider`, and this engine READS that
// resource correctly — `getResource`/`getResources` both find it (1 URL, same as the JVM). But
// `ServiceLoader.load(Provider.class)` then yields NOTHING: iterating it, `.stream().count()`
// and `StreamSupport.stream(sl.spliterator(), false).count()` are all 0 against the JVM's 1, so
// `ProviderUtil.lazyInit` sees no provider and falls back to the bundled `SimpleProvider` —
// which announces itself in these three lines. `reflect::functional::service_loader` and
// `reflect::resources::service_impls` implement the lookup and are never reached
// (`RAVA_TRACE_SVC=1` prints no `[SVC]` line for this call), so something upstream claims
// `ServiceLoader#iterator`/`#stream` first.
//
// NOT FIXED HERE, deliberately: making `ServiceLoader` work would hand Minecraft the REAL
// log4j-core backend in place of the no-op one it runs on today, which changes the logging
// output of every bench and gate in the tree. That is a separate change with its own
// measurement, not a side effect of a probe. The lines are timestamped, so they could never
// compare equal in any case.
// DIVERGES: [
// DIVERGES: Provider '
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import com.mojang.serialization.Codec;
import com.mojang.serialization.DataResult;
import com.mojang.serialization.JsonOps;
import com.mojang.serialization.codecs.RecordCodecBuilder;
import java.io.PrintStream;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.Optional;
import java.util.TreeMap;
import it.unimi.dsi.fastutil.objects.Reference2ObjectArrayMap;
import it.unimi.dsi.fastutil.objects.Reference2ObjectMap;
import it.unimi.dsi.fastutil.objects.Reference2ObjectMaps;
import net.minecraft.SharedConstants;
import net.minecraft.advancements.Advancement;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.component.DataComponentMap;
import net.minecraft.core.component.DataComponentPatch;
import net.minecraft.core.component.DataComponentType;
import net.minecraft.core.component.DataComponents;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.network.chat.Component;
import net.minecraft.resources.RegistryOps;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.item.Items;
import net.minecraft.world.item.crafting.Recipe;

public class CompSem {
    static PrintStream out;
    static void p(String k, Object v) { out.println(k + " " + v); }

    /** Sorted `class:key=value` lines so iteration order cannot leak into the comparison. */
    static String keys(Iterable<? extends Map.Entry<?, ?>> es) {
        List<String> xs = new ArrayList<>();
        for (Map.Entry<?, ?> e : es) {
            Object k = e.getKey();
            xs.add((k == null ? "null" : k.getClass().getName()) + ":" + k + "=" + e.getValue());
        }
        xs.sort(null);
        return xs.toString();
    }

    static String keys(Map<?, ?> m) { return keys(m.entrySet()); }

    /** Gson element with object keys sorted recursively (ReloadBench.Fold.canonical). */
    static String canonical(JsonElement e) {
        StringBuilder sb = new StringBuilder();
        canonical(e, sb);
        return sb.toString();
    }
    private static void canonical(JsonElement e, StringBuilder sb) {
        if (e == null || e.isJsonNull()) { sb.append("null"); return; }
        if (e.isJsonPrimitive()) { sb.append(e.toString()); return; }
        if (e.isJsonArray()) {
            sb.append('[');
            boolean first = true;
            for (JsonElement x : e.getAsJsonArray()) { if (!first) sb.append(','); first = false; canonical(x, sb); }
            sb.append(']');
            return;
        }
        TreeMap<String, JsonElement> m = new TreeMap<>();
        for (Map.Entry<String, JsonElement> en : e.getAsJsonObject().entrySet()) m.put(en.getKey(), en.getValue());
        sb.append('{');
        boolean first = true;
        for (Map.Entry<String, JsonElement> en : m.entrySet()) {
            if (!first) sb.append(','); first = false;
            sb.append('"').append(en.getKey()).append("\":");
            canonical(en.getValue(), sb);
        }
        sb.append('}');
    }

    static <T> String enc(Codec<T> codec, com.mojang.serialization.DynamicOps<JsonElement> ops, T v) {
        DataResult<JsonElement> r = codec.encodeStart(ops, v);
        return r.result().map(CompSem::canonical).orElseGet(() -> "ERR:" + r.error().map(Object::toString).orElse("?"));
    }

    static <T> T dec(Codec<T> codec, com.mojang.serialization.DynamicOps<JsonElement> ops, String json) {
        DataResult<T> r = codec.parse(ops, JsonParser.parseString(json));
        if (r.result().isPresent()) return r.result().get();
        p("decode.error", r.error().map(Object::toString).orElse("?"));
        return null;
    }

    public static void main(String[] args) {
        out = System.out;
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();

        // --- the door the builder uses: Reference2ObjectArrayMap.put with a registry object key -----
        DataComponentPatch built = DataComponentPatch.builder()
                .set(DataComponents.MAX_STACK_SIZE, 7)
                .set(DataComponents.DAMAGE, 3)
                .remove(DataComponents.LORE)
                .build();
        p("built.size", built.size());
        p("built.entries", keys(built.entrySet()));
        p("built.get", built.get(DataComponents.DAMAGE));
        p("built.getMissing", built.get(DataComponents.REPAIR_COST));
        p("built.toString", built);

        // --- the copy doors: the copy constructor, putAll and fastIterable over an ArrayMap --------
        Reference2ObjectMap<DataComponentType<?>, Optional<?>> src = new Reference2ObjectArrayMap<>();
        src.put(DataComponents.DAMAGE, Optional.of(5));
        src.put(DataComponents.CUSTOM_NAME, Optional.of(Component.literal("hi")));
        Reference2ObjectMap<DataComponentType<?>, Optional<?>> copy = new Reference2ObjectArrayMap<>(src);
        p("copy.entries", keys(copy));
        Reference2ObjectMap<DataComponentType<?>, Optional<?>> viaPutAll = new Reference2ObjectArrayMap<>(4);
        viaPutAll.putAll(src);
        p("putAll.entries", keys(viaPutAll));
        p("fastIterable.entries", keys(Reference2ObjectMaps.fastIterable(src)));
        p("keySet", keys(src.keySet().stream().map(k -> Map.entry(k, "k")).toList()));
        // --- Map.forEach is a BiConsumer over a MAP, never an Iterable drain --------------------
        List<String> seen = new ArrayList<>();
        src.forEach((k, v) -> seen.add(k.getClass().getName() + ":" + k + "=" + v));
        seen.sort(null);
        p("forEach", seen);
        seen.clear();
        Reference2ObjectMaps.fastForEach(src, e -> seen.add(e.getKey() + "=" + e.getValue()));
        seen.sort(null);
        p("fastForEach", seen);
        // --- the views WRITE THROUGH, like every other Map's --------------------------------------
        Reference2ObjectMap<DataComponentType<?>, Optional<?>> mut1 = new Reference2ObjectArrayMap<>(src);
        p("keySet.removeIf", mut1.keySet().removeIf(k -> k == DataComponents.DAMAGE) + " " + keys(mut1));
        Reference2ObjectMap<DataComponentType<?>, Optional<?>> mut2 = new Reference2ObjectArrayMap<>(src);
        p("keySet.remove", mut2.keySet().remove(DataComponents.CUSTOM_NAME) + " " + keys(mut2));
        Reference2ObjectMap<DataComponentType<?>, Optional<?>> mut3 = new Reference2ObjectArrayMap<>(src);
        p("entrySet.removeIf", mut3.entrySet().removeIf(e -> e.getValue().isPresent()) + " " + keys(mut3));
        Reference2ObjectMap<DataComponentType<?>, Optional<?>> mut4 = new Reference2ObjectArrayMap<>(src);
        p("values.remove", mut4.values().remove(Optional.of(5)) + " " + keys(mut4));
        Reference2ObjectMap<DataComponentType<?>, Optional<?>> mut5 = new Reference2ObjectArrayMap<>(src);
        p("keySet.retainAll", mut5.keySet().retainAll(List.of(DataComponents.DAMAGE)) + " " + keys(mut5));
        Reference2ObjectMap<DataComponentType<?>, Optional<?>> mut6 = new Reference2ObjectArrayMap<>(src);
        p("map.remove", mut6.remove(DataComponents.DAMAGE) + " " + keys(mut6));
        // --- the same map, the same shape, different contents: clear + refill ----------------------
        Reference2ObjectMap<DataComponentType<?>, Optional<?>> mut7 = new Reference2ObjectArrayMap<>(src);
        p("refill.before", keys(mut7));
        mut7.clear();
        mut7.put(DataComponents.LORE, Optional.empty());
        mut7.put(DataComponents.REPAIR_COST, Optional.of(1));
        p("refill.after", keys(mut7));
        mut7.put(DataComponents.REPAIR_COST, Optional.of(2));
        p("refill.overwrite", keys(mut7));
        // --- the SAM boundary: a boolean implementation behind an Object-returning site is a Boolean
        java.util.function.Function<DataComponentPatch, Object> isEmpty = DataComponentPatch::isEmpty;
        Object boxed = isEmpty.apply(built);
        p("sam.box", boxed.getClass().getName() + " " + Objects.equals(boxed, Boolean.FALSE) + " " + boxed);
        java.util.function.Function<DataComponentPatch, Object> size = DataComponentPatch::size;
        p("sam.int", size.apply(built).getClass().getName() + " " + Objects.equals(size.apply(built), 3));
        // --- optionalFieldOf(name, default) elides a default read through a record getter ---------
        record Flags(boolean a, boolean b, int n) {}
        Codec<Flags> flags = RecordCodecBuilder.create(i -> i.group(
                Codec.BOOL.optionalFieldOf("a", true).forGetter(Flags::a),
                Codec.BOOL.optionalFieldOf("b", false).forGetter(Flags::b),
                Codec.INT.optionalFieldOf("n", 3).forGetter(Flags::n)).apply(i, Flags::new));
        p("opt.defaults", enc(flags, JsonOps.INSTANCE, new Flags(true, false, 3)));
        p("opt.nondefault", enc(flags, JsonOps.INSTANCE, new Flags(false, true, 4)));
        // --- gson object equality, which `dispatchOptionalValue` uses to elide an empty value -----
        JsonObject empty = new JsonObject();
        JsonObject one = new JsonObject();
        one.addProperty("k", 1);
        JsonObject one2 = new JsonObject();
        one2.addProperty("k", 1);
        p("json.eq", empty.equals(new JsonObject()) + " " + one.equals(one2) + " " + one.equals(empty)
                + " " + (empty.hashCode() == new JsonObject().hashCode()));
        p("copy.get", copy.get(DataComponents.DAMAGE));
        p("copy.containsKey", copy.containsKey(DataComponents.CUSTOM_NAME));
        p("copy.equals", copy.equals(src));
        p("copy.hashEq", copy.hashCode() == src.hashCode());

        // --- the codec doors: decode builds the map from PatchKey.type(), encode reads it back -----
        String patchJson = "{\"minecraft:damage\":5,\"minecraft:custom_name\":\"hi\",\"!minecraft:lore\":{}}";
        DataComponentPatch decoded = dec(DataComponentPatch.CODEC, JsonOps.INSTANCE, patchJson);
        p("decoded.entries", keys(decoded.entrySet()));
        p("decoded.encode", enc(DataComponentPatch.CODEC, JsonOps.INSTANCE, decoded));
        p("built.encode", enc(DataComponentPatch.CODEC, JsonOps.INSTANCE, built));
        p("decoded.reencodeEq", enc(DataComponentPatch.CODEC, JsonOps.INSTANCE,
                dec(DataComponentPatch.CODEC, JsonOps.INSTANCE, patchJson)).equals(
                enc(DataComponentPatch.CODEC, JsonOps.INSTANCE, decoded)));
        p("split.set", keys(decoded.split().removed().stream().map(k -> Map.entry(k, "r")).toList()));
        DataComponentMap splitMap = decoded.split().added();
        p("split.map", splitMap.size() + " " + splitMap.get(DataComponents.DAMAGE));
        p("forget", keys(decoded.forget(t -> t == DataComponents.DAMAGE).entrySet()));

        // --- an ItemStack: PatchedDataComponentMap.fromPatch, asPatch, the copy-on-write copy -----
        String stackJson = "{\"id\":\"minecraft:diamond_sword\",\"count\":1,\"components\":{\"minecraft:damage\":5,\"minecraft:custom_name\":\"hi\"}}";
        ItemStack stack = dec(ItemStack.CODEC, JsonOps.INSTANCE, stackJson);
        p("stack.patch", keys(stack.getComponentsPatch().entrySet()));
        p("stack.encode", enc(ItemStack.CODEC, JsonOps.INSTANCE, stack));
        stack.set(DataComponents.REPAIR_COST, 2);
        p("stack.set.patch", keys(stack.getComponentsPatch().entrySet()));
        p("stack.set.encode", enc(ItemStack.CODEC, JsonOps.INSTANCE, stack));
        ItemStack copyStack = stack.copy();
        p("stack.copy.patch", keys(copyStack.getComponentsPatch().entrySet()));
        ItemStack fresh = new ItemStack(Items.DIAMOND_SWORD);
        fresh.applyComponents(stack.getComponentsPatch());
        p("stack.apply.patch", keys(fresh.getComponentsPatch().entrySet()));
        p("stack.apply.encode", enc(ItemStack.CODEC, JsonOps.INSTANCE, fresh));

        // --- a recipe whose result carries components: exactly ReloadBench's fold ----------------
        RegistryOps<JsonElement> ops = RegistryOps.create(JsonOps.INSTANCE,
                RegistryAccess.fromRegistryOfRegistries(BuiltInRegistries.REGISTRY));
        String recipeJson = "{\"type\":\"minecraft:crafting_shaped\",\"category\":\"misc\",\"pattern\":[\"# \",\" #\"],"
                + "\"key\":{\"#\":\"minecraft:paper\"},"
                + "\"result\":{\"id\":\"minecraft:firework_rocket\",\"count\":3,"
                + "\"components\":{\"minecraft:fireworks\":{\"explosions\":[],\"flight_duration\":1}}}}";
        Recipe<?> recipe = dec(Recipe.CODEC, ops, recipeJson);
        p("recipe.encode", enc(Recipe.CODEC, ops, recipe));
        p("recipe.encode2", enc(Recipe.CODEC, ops, recipe));
        String recipeJson2 = "{\"type\":\"minecraft:crafting_shapeless\",\"category\":\"misc\","
                + "\"ingredients\":[\"minecraft:paper\",\"minecraft:gunpowder\"],"
                + "\"result\":{\"id\":\"minecraft:diamond_sword\",\"components\":{\"minecraft:damage\":9,\"minecraft:custom_name\":\"x\"}}}";
        Recipe<?> recipe2 = dec(Recipe.CODEC, ops, recipeJson2);
        p("recipe2.encode", enc(Recipe.CODEC, ops, recipe2));
        // --- an advancement: three boolean display defaults and an empty conditions object ------
        String advJson = "{\"criteria\":{\"x\":{\"trigger\":\"minecraft:impossible\"}},\"requirements\":[[\"x\"]],"
                + "\"display\":{\"icon\":{\"id\":\"minecraft:stone\"},\"title\":\"t\",\"description\":\"d\",\"frame\":\"task\"}}";
        Advancement adv = dec(Advancement.CODEC, ops, advJson);
        p("adv.encode", enc(Advancement.CODEC, ops, adv));
        p("done", "ok");
    }
}
