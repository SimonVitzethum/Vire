public class Rc {
    public static void main(String[] args) {
        // Heap object, escapes (return) → refcount-managed
        Box b = make(21);
        System.out.println(b.v);        // 21

        // aliasing: c shares the object with b
        Box c = b;
        c.v = 99;
        System.out.println(b.v);        // 99 (same object)

        // field holds a reference to a nested object
        Box outer = new Box(1);
        outer.next = new Box(2);
        outer.next.next = new Box(3);
        System.out.println(outer.next.next.v);  // 3
        // outer, outer.next, outer.next.next: 4 heap boxes total with make/b

        // many short-lived objects in a loop (heap, because loop)
        int sum = 0;
        for (int i = 0; i < 1000; i++) {
            Box t = new Box(i);
            sum += t.v;
        }
        System.out.println(sum);        // 499500
    }

    static Box make(int x) {
        return new Box(x);
    }
}

class Box {
    int v;
    Box next;

    Box(int v) {
        this.v = v;
    }
}
