package net.minecraft.world.level.biome;

import com.mojang.datafixers.util.Pair;
import java.util.ArrayList;
import java.util.List;
import net.minecraft.resources.ResourceKey;

/** Steady-state speed of the climate R-tree search behind every biome lookup: the overworld's
 *  biome parameter list (OverworldBiomeBuilder), Climate.ParameterList.findValue on targets
 *  spread over the climate space. In the server jar's package for the builder's protected
 *  method; run with `-cp <classes>:<paper jar>:<libraries>`. */
public class RTreeBench {
    public static void main(String[] args) {
        net.minecraft.SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        List<Pair<Climate.ParameterPoint, ResourceKey<Biome>>> points = new ArrayList<>();
        new OverworldBiomeBuilder().addBiomes(points::add);
        Climate.ParameterList<ResourceKey<Biome>> list = new Climate.ParameterList<>(points);
        Climate.TargetPoint[] targets = new Climate.TargetPoint[4096];
        java.util.Random r = new java.util.Random(5);
        for (int i = 0; i < targets.length; i++) {
            targets[i] = Climate.target(r.nextFloat() * 2 - 1, r.nextFloat() * 2 - 1, r.nextFloat() * 2.4f - 1.2f, r.nextFloat() * 2 - 1, r.nextFloat() * 1.2f - 0.2f, r.nextFloat() * 2 - 1);
        }
        long check = 0;
        for (int round = 0; round < 8; round++) {
            long t0 = System.nanoTime();
            for (int k = 0; k < 100; k++) check += batch(list, targets);
            if (round >= 3) System.out.printf("%.1f ns per lookup%n", (System.nanoTime() - t0) / (100.0 * targets.length));
        }
        System.out.println("points " + points.size() + " check " + check);
    }

    static long batch(Climate.ParameterList<ResourceKey<Biome>> list, Climate.TargetPoint[] targets) {
        long h = 0;
        for (Climate.TargetPoint t : targets) h += list.findValue(t).location().getPath().length();
        return h;
    }
}
