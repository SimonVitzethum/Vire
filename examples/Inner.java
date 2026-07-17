public class Inner {
  int base=10;
  class Adder { int add(int x){ return base+x; } }
  int run(){ return new Adder().add(5); }
  public static void main(String[] a){ System.out.println(new Inner().run()); }
}
