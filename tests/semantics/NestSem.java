// Class.getNestMembers / getDeclaredClasses / getNestHost / isNestmateOf.
import java.util.Arrays;

public class NestSem {
    static class A { class Deep {} }
    interface B {}
    enum C { X }
    record D(int x) {}
    private static class E {}

    static String names(Class<?>[] cs) {
        return Arrays.stream(cs).map(Class::getName).sorted().toList().toString();
    }

    public static void main(String[] args) {
        Runnable lambda = () -> {};
        class Local {}
        System.out.println("members " + names(NestSem.class.getNestMembers()));
        System.out.println("members of A " + names(A.class.getNestMembers()));
        System.out.println("declared " + names(NestSem.class.getDeclaredClasses()));
        System.out.println("declared A " + names(A.class.getDeclaredClasses()));
        System.out.println("declared E " + names(E.class.getDeclaredClasses()));
        System.out.println("host " + A.Deep.class.getNestHost().getName() + " " + Local.class.getNestHost().getName());
        System.out.println("mates " + A.class.isNestmateOf(E.class) + " " + A.class.isNestmateOf(String.class));
        System.out.println("string members " + names(String.class.getNestMembers()).length());
        System.out.println("map entry declared " + names(java.util.Map.class.getDeclaredClasses()));
    }
}
