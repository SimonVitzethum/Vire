public class Trees {
    static class Node { Node l, r; }
    static int check(Node n) { return n.l == null ? 1 : 1 + check(n.l) + check(n.r); }
    static Node make(int d) {
        Node n = new Node();
        if (d > 0) { n.l = make(d - 1); n.r = make(d - 1); }
        return n;
    }
    public static void main(String[] args) {
        int maxDepth = 18; long sum = 0;
        for (int depth = 4; depth <= maxDepth; depth += 2) {
            int iterations = 1 << (maxDepth - depth + 4);
            long chk = 0;
            for (int i = 0; i < iterations; i++) chk += check(make(depth));
            sum += chk;
        }
        System.out.println(sum);
    }
}
