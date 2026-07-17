public class Seal {
  sealed interface Shape permits Circle,Square {}
  record Circle(int r) implements Shape {}
  record Square(int s) implements Shape {}
  static int area(Shape sh){ return switch(sh){ case Circle c -> c.r()*c.r()*3; case Square s -> s.s()*s.s(); }; }
  public static void main(String[] a){ System.out.println(area(new Circle(2))+area(new Square(3))); }
}
