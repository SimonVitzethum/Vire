public class Floats {
    public static void main(String[] args) {
        float a = 3.5f;
        float b = 2.0f;
        System.out.println("a+b = " + (a + b));   // 5.5
        System.out.println("a*b = " + (a * b));    // 7
        System.out.println("a/b = " + (a / b));    // 1.75

        // Konvertierungen
        int i = 10;
        float fi = i;           // i2f
        double d = fi;          // f2d
        float df = (float) 3.14159; // d2f
        int back = (int) (a * b);   // f2i
        System.out.println("fi = " + fi);       // 10
        System.out.println("df = " + df);       // 3.14159
        System.out.println("back = " + back);   // 7

        // Vergleich
        System.out.println(a > b ? 1 : 0);      // 1

        // Float-Wrapper (Autoboxing)
        Float boxed = 1.5f;
        float unb = boxed;
        System.out.println("boxed = " + boxed); // 1.5
        System.out.println("unb+1 = " + (unb + 1.0f)); // 2.5
    }
}
