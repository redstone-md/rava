public class HelperLoop {
  static long compute(int n){ long h=1125899906842597L; for(int i=1;i<=n;i++){ h=h*31+(i%7); h^=(i*2654435761L);} return h; }
  public static void main(String[] a){ System.out.println(compute(Integer.parseInt(a[0]))); }
}
