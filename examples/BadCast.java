class A { int x = 1; }
class B { int y = 2; }
public class BadCast {
    public static void main(String[] args) {
        MiniList<Object> list = new MiniList<>();
        list.add(new A());
        Object o = list.get(0);
        B b = (B) o;              // ClassCastException (A ist kein B)
        System.out.println(b.y);
    }
}
