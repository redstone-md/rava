public class ExcGcRepro {
    public static void main(String[] a) {
        long d = 0;
        for (int i = 1; i <= 5000; i++) {
            // allocation pressure to provoke GC around the throw/catch window
            StringBuilder sb = new StringBuilder(); for (int j=0;j<50;j++) sb.append(j).append('.');
            String junk = sb.toString(); d += junk.length();
            try {
                if (i % 50 == 0) throw new RuntimeException("msg" + (i%7));
            } catch (RuntimeException e) {
                String m = e.getMessage();               // must NOT be null
                d ^= (m == null ? -999 : m.hashCode());
            }
        }
        System.out.println("D=" + d);
        System.out.println("DONE");
    }
}
