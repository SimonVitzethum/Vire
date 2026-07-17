public class Quick {
    static void sort(int[] a, int lo, int hi) {
        while (lo < hi) {
            int p = a[(lo + hi) >>> 1], i = lo, j = hi;
            while (i <= j) {
                while (a[i] < p) i++;
                while (a[j] > p) j--;
                if (i <= j) { int t = a[i]; a[i] = a[j]; a[j] = t; i++; j--; }
            }
            if (j - lo < hi - i) { sort(a, lo, j); lo = i; }
            else { sort(a, i, hi); hi = j; }
        }
    }
    public static void main(String[] args) {
        int n = 20_000_000; int[] a = new int[n];
        long s = 12345;
        for (int i = 0; i < n; i++) { s = s * 6364136223846793005L + 1442695040888963407L; a[i] = (int) (s >>> 33); }
        sort(a, 0, n - 1);
        long sum = 0; for (int i = 0; i < n; i += 1000) sum += a[i];
        System.out.println(sum + " " + a[0] + " " + a[n-1]);
    }
}
