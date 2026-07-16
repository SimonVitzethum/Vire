public class Cycle {
    public static void main(String[] args) {
        Box a = new Box(1);
        Box b = new Box(2);
        a.next = b;   // a -> b
        b.next = a;   // b -> a : Zyklus, Refcounting kann das nicht einsammeln
        System.out.println(a.next.v + b.next.v);  // 2 + 1 = 3
    }
}
