import it.unimi.dsi.fastutil.longs.Long2ObjectLinkedOpenHashMap;

public class LruMap {
    static long key(int x, int z) {
        return (x & 0xFFFFFFFFL) | ((z & 0xFFFFFFFFL) << 32);
    }
    public static void main(String[] a) {
        Long2ObjectLinkedOpenHashMap<String> cache = new Long2ObjectLinkedOpenHashMap<>();
        int[][] regions = {{-1,-1},{-1,0},{0,-1},{0,0}};
        for (int[] r : regions) {
            long k = key(r[0], r[1]);
            String hit = cache.getAndMoveToFirst(k);
            System.out.println("first-get r." + r[0] + "." + r[1] + " -> " + hit);
            cache.putAndMoveToFirst(k, "file:" + r[0] + "." + r[1]);
        }
        for (int[] r : regions) {
            System.out.println("re-get r." + r[0] + "." + r[1] + " -> "
                + cache.getAndMoveToFirst(key(r[0], r[1])));
        }
        System.out.println("SIZE=" + cache.size());
    }
}
