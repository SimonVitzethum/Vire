public class Rec {
  record Point(int x, int y) {}
  public static void main(String[] a){
    Point p = new Point(3, 4);
    System.out.println(p);            // toString
    System.out.println(p.x());
    System.out.println(p.equals(new Point(3,4)));
    System.out.println(p.hashCode() != 0);
  }
}
