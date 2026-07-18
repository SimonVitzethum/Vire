public class ArrExc {
    public static void main(String[] args) {
        int[] a = new int[5];
        for (int i = 0; i < 5; i++) a[i] = i * i;

        // catchable bounds
        System.out.println(safeGet(a, 2));    // 4
        System.out.println(safeGet(a, 10));   // -1 (caught)

        // NPE on null array is catchable
        int[] n = null;
        try {
            int x = n[0];
            System.out.println("not reached " + x);
        } catch (NullPointerException e) {
            System.out.println("null array caught");
        }

        // arraylength on null
        try {
            System.out.println(n.length);
        } catch (RuntimeException e) {
            System.out.println("length on null caught");
        }

        System.out.println("continuing");
    }

    static int safeGet(int[] arr, int i) {
        try {
            return arr[i];
        } catch (ArrayIndexOutOfBoundsException e) {
            return -1;
        }
    }
}
