class Node { int v; Node(int v){this.v=v;} }
public class LoopCarry {
    public static void main(String[] args) {
        Node prev = null; long s = 0;
        for (int i = 0; i < 6; i++) {
            Node n = new Node(i);
            if (prev != null) s += prev.v;   // reads previous-iteration object
            prev = n;
        }
        System.out.println(s);   // 0+1+2+3+4 = 10
    }
}
