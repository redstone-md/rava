package dev.rava.patches.targeting;

import java.util.function.Predicate;
import net.minecraft.world.entity.LivingEntity;
import net.minecraft.world.entity.ai.targeting.TargetingConditions;

/**
 * `TargetingConditions.test` for EntityGetter's searches. Vanilla rejects a target when it
 * cannot be seen by anyone, or when the condition's selector rejects it, in that order; a
 * goal's selector is usually a cheap type test that rejects nearly every neighbour, while
 * `canBeSeenByAnyone` carries mods' hooks. Both are predicates without side effects, so asking
 * the selector first gives the same answer: a rejected target is rejected at once, any other
 * gets the whole vanilla test.
 */
public final class Targeting {
    /** `-Drava.targeting.verify=true`: also run the vanilla test and report any difference. */
    private static final boolean VERIFY = Boolean.getBoolean("rava.targeting.verify");
    private static long checked, differed;

    private Targeting() {}

    public static boolean test(TargetingConditions conditions, LivingEntity attacker, LivingEntity target) {
        Predicate<LivingEntity> selector = conditions.selector;
        if (selector != null && !selector.test(target)) {
            if (VERIFY) {
                verify(conditions, attacker, target);
            }
            return false;
        }
        return conditions.test(attacker, target);
    }

    private static void verify(TargetingConditions conditions, LivingEntity attacker, LivingEntity target) {
        checked++;
        if (conditions.test(attacker, target) && differed++ < 20) {
            System.err.println("[targeting] " + target + " passes vanilla but its selector rejected it");
        }
        if ((checked & ((1 << 22) - 1)) == 0) {
            System.err.println("[targeting] verified " + checked + " rejections, " + differed + " differed");
        }
    }
}
