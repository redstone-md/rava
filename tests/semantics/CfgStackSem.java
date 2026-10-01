public class CfgStackSem {
    int value;
    CfgStackSem(int value) { this.value = value; }

    static int bool(int a, int b) {
        int result = 0;
        for (int i = 0; i < 1; i++) result = a != 0 || b != 0 ? 1 : 0;
        return result;
    }
    static Object reference(boolean choose, Object a, Object b) {
        Object result = null;
        for (int i = 0; i < 1; i++) result = choose ? a : b;
        return result;
    }
    static long wide(boolean choose, long a, long b) {
        long result = 0;
        for (int i = 0; i < 1; i++) result = choose ? a : b;
        return result;
    }
    static int dereference(boolean choose, CfgStackSem a, CfgStackSem b) {
        int result = 0;
        for (int i = 0; i < 1; i++) result = (choose ? a : b).value;
        return result;
    }
    static int switchValue(int index) {
        return 40 + switch (index) { case 0 -> 1; case 1 -> 2; default -> 3; };
    }
    public static void main(String[] args) {
        for (int a = 0; a < 2; a++) for (int b = 0; b < 2; b++)
            System.out.println("bool=" + a + "," + b + ":" + bool(a, b));
        Object marker = new Object();
        System.out.println("ref-left=" + (reference(true, marker, null) == marker));
        System.out.println("ref-right=" + (reference(false, null, marker) == marker));
        System.out.println("ref-null=" + (reference(true, null, marker) == null));
        System.out.println("wide-left=" + wide(true, Long.MIN_VALUE, Long.MAX_VALUE));
        System.out.println("wide-right=" + wide(false, Long.MIN_VALUE, Long.MAX_VALUE));
        CfgStackSem object = new CfgStackSem(77);
        System.out.println("unused-null=" + dereference(true, object, null));
        try { dereference(false, object, null); System.out.println("missing-fault"); }
        catch (NullPointerException e) { System.out.println("selected-null=NullPointerException"); }
        for (int index = -1; index < 3; index++) System.out.println("switch=" + switchValue(index));
    }
}
