public class Cycle3 {
    public static void main(String[] args) {
        // three-way cycle a->b->c->a, plus an external chain that is acyclic
        Box a = new Box(1), b = new Box(2), c = new Box(3);
        a.next = b; b.next = c; c.next = a;
        System.out.println(a.next.next.next.v);  // back at a: 1

        // mixed: long-lived chain (no cycle) + discarded cycle
        for (int i = 0; i < 500; i++) {
            Box x = new Box(i), y = new Box(i);
            x.next = y; y.next = x;   // one cycle each, immediately unreachable
        }
        System.out.println(42);
    }
}
