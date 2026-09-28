public class ConcatRepro {
    static final class Point {
        final int x, y; Point(int x,int y){this.x=x;this.y=y;}
        public String toString(){ return "(" + x + "," + y + ")"; }
    }
    public static void main(String[] a) {
        // exercise the exact values StringBench uses, print string + len + hash
        int[] xs = {0, 1, 313, 996, 42, 100};
        long checkLen = 0, checkHash = 0;
        for (int i : xs) {
            Point p = new Point(i % 997, (i * 7) % 1013);
            String s = String.valueOf(p);          // Object overload -> toString
            System.out.println("P(" + i + ")=[" + s + "] len=" + s.length() + " hash=" + s.hashCode());
            checkLen += s.length(); checkHash ^= s.hashCode();
        }
        System.out.println("checkLen=" + checkLen + " checkHash=" + checkHash);
        // isolate concat forms
        int x = 313, y = 331;
        String c1 = "(" + x + "," + y + ")";
        StringBuilder sb = new StringBuilder(); sb.append('(').append(x).append(',').append(y).append(')');
        String c2 = sb.toString();
        System.out.println("indyConcat=[" + c1 + "] hash=" + c1.hashCode());
        System.out.println("sbConcat  =[" + c2 + "] hash=" + c2.hashCode());
        System.out.println("valueOfInt=[" + String.valueOf(x) + "] Integer.toString=[" + Integer.toString(x) + "]");
        // repeated concat producing large negative/positive ints
        long h = 0; for (int i = 0; i < 5; i++) { String t = "v" + (i*7) + "_" + (i%3); h ^= t.hashCode(); }
        System.out.println("repeatHash=" + h);
        System.out.println("DONE");
    }
}
