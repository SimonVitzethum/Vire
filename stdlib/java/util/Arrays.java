package java.util;

// Teil-Stub von java.util.Arrays für die Closed-World-Kompilierung. Nutzt die
// neuen Intrinsics System.arraycopy/Math.min sowie StringBuilder. Bewusst
// klein: die gängigen int[]-/Object[]-Operationen.
public class Arrays {
    public static void fill(int[] a, int v) {
        for (int i = 0; i < a.length; i++) a[i] = v;
    }

    public static int[] copyOf(int[] a, int n) {
        int[] r = new int[n];
        System.arraycopy(a, 0, r, 0, Math.min(a.length, n));
        return r;
    }

    public static Object[] copyOf(Object[] a, int n) {
        Object[] r = new Object[n];
        System.arraycopy(a, 0, r, 0, Math.min(a.length, n));
        return r;
    }

    // Insertion Sort (klein, stabil) — ausreichend für den Stub.
    public static void sort(int[] a) {
        for (int i = 1; i < a.length; i++) {
            int k = a[i], j = i - 1;
            while (j >= 0 && a[j] > k) {
                a[j + 1] = a[j];
                j--;
            }
            a[j + 1] = k;
        }
    }

    public static String toString(int[] a) {
        if (a == null) return "null";
        StringBuilder sb = new StringBuilder();
        sb.append("[");
        for (int i = 0; i < a.length; i++) {
            if (i > 0) sb.append(", ");
            sb.append(a[i]);
        }
        sb.append("]");
        return sb.toString();
    }
}
