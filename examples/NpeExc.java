class Node { int val; Node next; Node(int v) { val = v; } int get() { return val; } }
public class NpeExc {
    public static void main(String[] args) {
        Node a = new Node(5);
        System.out.println(a.val);       // 5 (Feld, non-null)

        // Feld-NPE abfangbar
        Node n = null;
        try {
            int x = n.val;
            System.out.println("nicht erreicht " + x);
        } catch (NullPointerException e) {
            System.out.println("feld-npe gefangen");
        }

        // putfield-NPE abfangbar
        try {
            n.val = 10;
        } catch (NullPointerException e) {
            System.out.println("putfield-npe gefangen");
        }

        // Receiver-NPE (Methodenaufruf) abfangbar
        try {
            int y = n.get();
            System.out.println("nicht erreicht " + y);
        } catch (NullPointerException e) {
            System.out.println("receiver-npe gefangen");
        }

        System.out.println("weiter");
    }
}
