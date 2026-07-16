// Feld-sensitive Escape-Analyse: verschachtelte lokale Objektgraphen landen
// komplett auf dem Stack (both-or-neither), während entkommende Container
// ihre Inhalte korrekt im Heap halten. Die Heap-Bilanz (0 live) ist der
// eigentliche Soundness-Test.
class Node2 { int v; Node2 link; }

public class EscapeFields {
    static Node2 keep;

    // Container entkommt (return) → Inhalt muss Heap bleiben (sonst dangling).
    static Node2 escaping() {
        Node2 head = new Node2();
        head.link = new Node2();
        head.link.v = 7;
        return head;
    }

    // Alles lokal, über Felder verkettet → gesamte Komponente auf den Stack.
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
