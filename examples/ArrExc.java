public class ArrExc {
    public static void main(String[] args) {
        int[] a = new int[5];
        for (int i = 0; i < 5; i++) a[i] = i * i;

        // abfangbare Bounds
        System.out.println(safeGet(a, 2));    // 4
        System.out.println(safeGet(a, 10));   // -1 (gefangen)

        // NPE auf null-Array abfangbar
        int[] n = null;
        try {
            int x = n[0];
            System.out.println("nicht erreicht " + x);
        } catch (NullPointerException e) {
            System.out.println("null-array gefangen");
        }

        // arraylength auf null
        try {
            System.out.println(n.length);
        } catch (RuntimeException e) {
            System.out.println("length auf null gefangen");
        }

        System.out.println("weiter");
    }

    static int safeGet(int[] arr, int i) {
        try {
            return arr[i];
        } catch (ArrayIndexOutOfBoundsException e) {
            return -1;
        }
    }
}
