public class Cmp {
  static <T extends Comparable<T>> T max(T a, T b){ return a.compareTo(b)>=0?a:b; }
  public static void main(String[] x){
    System.out.println(max(3, 7));
    System.out.println(max("apple","banana"));
  }
}
