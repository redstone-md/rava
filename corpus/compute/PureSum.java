public class PureSum { public static void main(String[] a){
  int n=Integer.parseInt(a[0]); long acc=0;
  for(int i=0;i<n;i++){ acc += (long)i; }
  System.out.println(acc);
} }
