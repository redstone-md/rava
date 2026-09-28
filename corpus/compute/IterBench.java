import java.util.*;
public class IterBench {
  public static void main(String[] a){
    int n=Integer.parseInt(a[0]);
    ArrayList<Integer> list = new ArrayList<>();
    for(int i=0;i<1000;i++) list.add(i*7+3);
    long acc=0;
    for(int r=0;r<n;r++){ for(Integer x : list){ acc += x; acc ^= (x*31); } }
    System.out.println(acc);
  }
}
