// Field-sensitive escape analysis: nested local object graphs end up
// entirely on the stack (both-or-neither), while escaping containers
// correctly keep their contents on the heap. The heap balance (0 live) is
// the actual soundness test.
class Node2 { int v; Node2 link; }

public class EscapeFields {
    static Node2 keep;

    // Container escapes (return) → content must stay on heap (else dangling).
    static Node2 escaping() {
        Node2 head = new Node2();
        head.link = new Node2();
        head.link.v = 7;
        return head;
    }

    // All local, linked via fields → entire component on the stack.
    static int local() {
        Node2 a = new Node2();
        Node2 b = new Node2();
        a.link = b;
        b.v = 5;
        a.v = 1;
        return a.v + a.link.v;
    }

    public static void main(String[] args) {
        Node2 e = escaping();
        keep = e;
        System.out.println(e.link.v);   // 7
        System.out.println(local());    // 6
        keep = null;
    }
}
