public class SumH {
  static long sum(int n){ long acc=0; for(int i=0;i<n;i++){ acc += (long)i*2654435761L + (i^0x5bd1e995); } return acc; }
  public static void main(String[] a){ System.out.println(sum(Integer.parseInt(a[0]))); }
}
