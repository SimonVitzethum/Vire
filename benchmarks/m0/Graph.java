public class Graph {
    static class Node {
        double rank, next;
        int outdeg;
        Node[] out;          // Nachbar-Referenzen: geteilt (Aliasing), zyklenfähig
    }
    public static void main(String[] args) {
        int N = 200_000, E = 6;
        Node[] nodes = new Node[N];
        for (int i = 0; i < N; i++) { nodes[i] = new Node(); nodes[i].rank = 1.0 / N; }
        long s = 88172645463325252L;
        for (int i = 0; i < N; i++) {
            Node n = nodes[i];
            n.out = new Node[E];
            for (int k = 0; k < E; k++) {
                s ^= s << 13; s ^= s >>> 7; s ^= s << 17;      // xorshift
                int j = (int) ((s >>> 1) % N);
                n.out[k] = nodes[j];                            // geteilte Referenz
            }
            n.outdeg = E;
        }
        double d = 0.85, base = (1.0 - d) / N;
        for (int it = 0; it < 40; it++) {                       // mutiert geteilten Zustand
            for (int i = 0; i < N; i++) nodes[i].next = base;
            for (int i = 0; i < N; i++) {
                Node n = nodes[i];
                double share = d * n.rank / n.outdeg;
                Node[] out = n.out;
                for (int k = 0; k < out.length; k++) out[k].next += share;
            }
            for (int i = 0; i < N; i++) nodes[i].rank = nodes[i].next;
        }
        double sum = 0; for (int i = 0; i < N; i++) sum += nodes[i].rank;
        System.out.println((long) (sum * 1_000_000.0));
    }
}
