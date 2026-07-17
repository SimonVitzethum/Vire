class Node { int v; Node(int v){this.v=v;} }
public class BenchAlloc {
    public static void main(String[] args) {
        long s = 0;
        for (int i = 0; i < 50000000; i++) { Node n = new Node(i); s += n.v; }
        System.out.println(s);
    }
}
