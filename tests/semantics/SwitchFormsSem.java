// Every way a switch dispatches in compiled code: a dense one through a jump table (a range
// starting below zero, gaps falling to the default, keys outside the range on both sides,
// sign-extended byte keys), a long sparse one by binary search (extreme keys), a string switch
// (hash codes) and an enum switch (ordinals through the synthetic map).
public class SwitchFormsSem {
    enum Dir { NORTH, EAST, SOUTH, WEST, UP, DOWN, IN, OUT }

    static int dense(int k) {
        switch (k) {
            case -3: return 11; case -2: return 12; case -1: return 13; case 0: return 14;
            case 1: return 15; case 2: return 16; case 4: return 18; case 5: return 19;
            case 6: return 20; case 8: return 22; case 9: return 23; case 10: return 24;
            default: return -1;
        }
    }

    static int sparse(int k) {
        switch (k) {
            case Integer.MIN_VALUE: return 1; case -100000: return 2; case -77: return 3; case -1: return 4;
            case 3: return 5; case 64: return 6; case 999: return 7; case 4096: return 8;
            case 70000: return 9; case 1 << 20: return 10; case 123456789: return 11; case Integer.MAX_VALUE: return 12;
            default: return 0;
        }
    }

    static int bytes(byte b) {
        switch (b) {
            case -128: return 1; case -2: return 2; case -1: return 3; case 0: return 4;
            case 1: return 5; case 2: return 6; case 3: return 7; case 127: return 8;
            default: return 9;
        }
    }

    static int chars(char c) {
        switch (c) {
            case 'a': return 1; case 'b': return 2; case 'c': return 3; case 'd': return 4;
            case 'e': return 5; case 'f': return 6; case '￿': return 7;
            default: return 0;
        }
    }

    static int strings(String s) {
        switch (s) {
            case "north": return 1; case "east": return 2; case "south": return 3; case "west": return 4;
            case "up": return 5; case "down": return 6; case "Aa": return 7; case "BB": return 8;
            default: return 0;
        }
    }

    static int enums(Dir d) {
        switch (d) {
            case NORTH: return 3; case EAST: return 5; case SOUTH: return 7; case WEST: return 11;
            case UP: return 13; case DOWN: return 17;
            default: return 19;
        }
    }

    public static void main(String[] a) {
        int[] keys = { Integer.MIN_VALUE, -100000, -77, -5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 64, 999, 4096, 70000, 1 << 20, 123456789, Integer.MAX_VALUE };
        String[] words = { "north", "east", "south", "west", "up", "down", "Aa", "BB", "x", "" };
        Dir[] dirs = Dir.values();
        long sum = 0;
        for (int round = 0; round < 30_000; round++) {
            for (int k : keys) sum = sum * 31 + dense(k) * 7 + sparse(k);
            for (int b = -128; b < 128; b += 3) sum += bytes((byte) b) * (b & 7);
            for (char c : new char[] { 'a', 'c', 'f', 'g', '￿', '\u0000' }) sum += chars(c);
            for (String w : words) sum += strings(w);
            for (Dir d : dirs) sum += enums(d);
        }
        System.out.println("sum " + sum);
        StringBuilder sb = new StringBuilder();
        for (int k : keys) sb.append(dense(k)).append('/').append(sparse(k)).append(' ');
        System.out.println(sb);
    }
}
