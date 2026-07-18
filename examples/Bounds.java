// Tests bounds-check elision: counted loops over `new T[n]` (index
// provably in [0,n)) must run unchecked+correctly, unprovable accesses
// stay checked and throw/catch. Result bit-identical to the JVM.
public class Bounds {
    public static void main(String[] a) {
        int n = 1000;
        int[] arr = new int[n];
        for (int i = 0; i < n; i++) arr[i] = i * i;      // elided
        long s = 0;
        for (int i = 0; i < arr.length; i++) s += arr[i]; // elided (arr.length bound)
        System.out.println(s);                            // 332833500

        // Long induction + (int) cast (sieve pattern): elided.
        boolean[] c = new boolean[n];
        long hits = 0;
        for (int i = 2; i < n; i++)
            for (long j = (long) i * i; j < n; j += i) { if (!c[(int) j]) hits++; c[(int) j] = true; }
        System.out.println(hits);                         // 830

        // Unprovable index (parameter) stays checked → catchable.
        System.out.println(safe(arr, 500));               // 250000
        System.out.println(safe(arr, 5000));              // -1 (caught)
    }

    static int safe(int[] arr, int i) {
        try {
            return arr[i];
        } catch (ArrayIndexOutOfBoundsException e) {
            return -1;
        }
    }
}
