public class Cycle3 {
    public static void main(String[] args) {
        // Dreier-Zyklus a->b->c->a, plus externe Kette die azyklisch ist
        Box a = new Box(1), b = new Box(2), c = new Box(3);
        a.next = b; b.next = c; c.next = a;
        System.out.println(a.next.next.next.v);  // zurück bei a: 1

        // gemischt: langlebige Kette (kein Zyklus) + weggeworfener Zyklus
        for (int i = 0; i < 500; i++) {
            Box x = new Box(i), y = new Box(i);
            x.next = y; y.next = x;   // je ein Zyklus, sofort unerreichbar
        }
        System.out.println(42);
    }
}
