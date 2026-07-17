import java.util.*;
public class GenMax {
  static <T extends Comparable<T>> T max(List<T> xs){
    T m=xs.get(0);
    for(T x:xs) if(x.compareTo(m)>0) m=x;
    return m;
  }
  public static void main(String[] a){
    List<Integer> xs=new ArrayList<>(); xs.add(3); xs.add(9); xs.add(1);
    System.out.println(max(xs));
  }
}
