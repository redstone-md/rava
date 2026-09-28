public class SumS {
  static long sum(int n){ long acc=0; for(int i=0;i<n;i++){ acc += i*7 + (i&15); } return acc; }
  public static void main(String[] a){ System.out.println(sum(Integer.parseInt(a[0]))); }
}
