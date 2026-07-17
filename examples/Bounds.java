// Testet Bounds-Check-Elision: gezählte Schleifen über `new T[n]` (Index
// beweisbar in [0,n)) müssen unchecked+korrekt laufen, unbeweisbare Zugriffe
// weiter geprüft werfen/fangen. Ergebnis bit-gleich zur JVM.
public class Bounds {
    public static void main(String[] a) {
        int n = 1000;
        int[] arr = new int[n];
        for (int i = 0; i < n; i++) arr[i] = i * i;      // elidiert
        long s = 0;
        for (int i = 0; i < arr.length; i++) s += arr[i]; // elidiert (arr.length-Schranke)
        System.out.println(s);                            // 332833500

        // Long-Induktion + (int)-Cast (Sieb-Muster): elidiert.
        boolean[] c = new boolean[n];
        long hits = 0;
        for (int i = 2; i < n; i++)
            for (long j = (long) i * i; j < n; j += i) { if (!c[(int) j]) hits++; c[(int) j] = true; }
        System.out.println(hits);                         // 830

        // Unbeweisbarer Index (Parameter) bleibt geprüft → abfangbar.
        System.out.println(safe(arr, 500));               // 250000
        System.out.println(safe(arr, 5000));              // -1 (gefangen)
    }

    static int safe(int[] arr, int i) {
        try {
            return arr[i];
        } catch (ArrayIndexOutOfBoundsException e) {
            return -1;
        }
    }
}
