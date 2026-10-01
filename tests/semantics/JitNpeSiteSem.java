public class JitNpeSiteSem {
    static int after;
    static String field;
    static int localLength() { String s = null; int n = s.length(); after++; return n; }
    static void fieldHash() { int n = field.hashCode(); after += n + 1; }
    public static void main(String[] args) {
        int caught = 0;
        String length = "", hash = "";
        for (int i = 0; i < 4096; i++) {
            try { localLength(); }
            catch (NullPointerException e) { caught++; length = e.getMessage(); }
            try { fieldHash(); }
            catch (NullPointerException e) { caught++; hash = e.getMessage(); }
        }
        System.out.println("caught=" + caught);
        System.out.println("after=" + after);
        System.out.println("length=" + length);
        System.out.println("hash=" + hash);
    }
}
