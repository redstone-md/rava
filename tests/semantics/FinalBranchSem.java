// Final fields read in a loop that calls out: hoisted when every iteration reads them, kept in
// place when only a branch does (the branch may select which object, of which class, is read).
public class FinalBranchSem {
    interface Src { int get(int i); }
    static final class Arr implements Src { final int[] data; Arr(int n) { data = new int[n]; for (int i = 0; i < n; i++) data[i] = i * 3; } public int get(int i) { return data[i % data.length]; } }
    static final class Pair implements Src { final int a, b; Pair(int a, int b) { this.a = a; this.b = b; } public int get(int i) { return (i & 1) == 0 ? a : b; } }
    static final class Box { final Src src; final String name; Box(Src s, String n) { src = s; name = n; } }
    static int calls;
    static void note(int i) { if ((i & 255) == 0) calls++; }

    static long walk(Box[] boxes, Box fixed, int n) {
        long s = 0;
        for (int i = 0; i < n; i++) {
            note(i);
            s += fixed.name.length();
            Box b = boxes[i % boxes.length];
            if (b.src instanceof Arr) s += ((Arr) b.src).data.length + b.src.get(i);
            else if (b.src instanceof Pair) s += ((Pair) b.src).a * 7 + b.src.get(i);
            if ((i % 3) == 0) s += fixed.src.get(i);
        }
        return s;
    }

    public static void main(String[] args) {
        Box[] boxes = {new Box(new Arr(5), "a"), new Box(new Pair(2, 9), "bb"), new Box(new Arr(9), "ccc")};
        long h = 0;
        for (int r = 0; r < 20000; r++) h = h * 31 + walk(boxes, boxes[r % 3], 50);
        System.out.println(h + " " + calls);
    }
}
