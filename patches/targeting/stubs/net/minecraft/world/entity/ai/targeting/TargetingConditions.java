package net.minecraft.world.entity.ai.targeting;

import java.util.function.Predicate;
import net.minecraft.world.entity.LivingEntity;

/** Compile-time view of the members the patch opens; never packaged. */
public class TargetingConditions {
    public Predicate<LivingEntity> selector;

    public boolean test(LivingEntity attacker, LivingEntity target) {
        throw new AssertionError();
    }
}
