// Regression for the collector: build a long linked list by prepend (each `head =
// x` displaces the previous head, buffering it as a possible-root at rc>0), then
// drop the whole list. The [heap] oracle must report 0 still live. This pattern —
// many buffered live roots, then a bulk drop — is what an incorrect incremental
// collector leaks on (it caught exactly such a bug); the synchronous collector is
// clean. See crates/driver/src/runtime.c.
public class ListDrop {
    static class N { N next; int v; }
    public static void main(String[] a) {
        N head = null;
        for (int i = 0; i < 200000; i++) { N x = new N(); x.next = head; x.v = i; head = x; }
        head = null;   // drop the whole list
        System.out.println(1);
    }
}
