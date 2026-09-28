public class JitStringNullSem {
    static int after;
    static int length(String value) { int n = value.length(); after++; return n; }
    static int hash(String value) { int n = value.hashCode(); after++; return n; }
    public static void main(String[] args) {
        int lengthCaught = 0, hashCaught = 0;
        for (int i = 0; i < 4096; i++) {
            length("hello"); hash("hello");
            try { length(null); } catch (NullPointerException e) { lengthCaught++; }
            try { hash(null); } catch (NullPointerException e) { hashCaught++; }
        }
        System.out.println("length=" + lengthCaught);
        System.out.println("hash=" + hashCaught);
        System.out.println("after=" + after);
    }
}
