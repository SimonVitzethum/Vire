class Node { int val; Node next; Node(int v) { val = v; } int get() { return val; } }
public class NpeExc {
    public static void main(String[] args) {
        Node a = new Node(5);
        System.out.println(a.val);       // 5 (field, non-null)

        // field NPE catchable
        Node n = null;
        try {
            int x = n.val;
            System.out.println("not reached " + x);
        } catch (NullPointerException e) {
            System.out.println("field-npe caught");
        }

        // putfield NPE catchable
        try {
            n.val = 10;
        } catch (NullPointerException e) {
            System.out.println("putfield-npe caught");
        }

        // receiver NPE (method call) catchable
        try {
            int y = n.get();
            System.out.println("not reached " + y);
        } catch (NullPointerException e) {
            System.out.println("receiver-npe caught");
        }

        System.out.println("continue");
    }
}
