// Incremental cycle collector under load: 300k self-referential objects created
// in a loop, each stored to a static field (forces the RC heap, not the loop
// arena) and dropped the next iteration. This buffers far more candidate roots
// than ROOTS_SOFT_CAP, so jrt_collect_step runs continuously mid-execution — the
// [heap] oracle must report 0 still live (all cycles reclaimed). See
// crates/driver/src/runtime.c (jrt_collect_step).
public class CycleStress {
    static class N { N next; }
    static N[] sink = new N[1];
    public static void main(String[] a) {
        for (int i = 0; i < 300000; i++) {
            N x = new N();
            x.next = x;      // self-cycle → needs the trial-deletion collector
            sink[0] = x;     // escapes to a static field → heap, not arena
        }
        sink[0] = null;
    }
}
