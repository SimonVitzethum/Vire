public class Nums {
    public static void main(String[] args) {
        long a = 1000000000L;
        long b = a * a;                 // 10^18, überläuft int
        System.out.print("a*a = ");
        System.out.println(b);          // 1000000000000000000

        long fac = 1;
        for (int i = 1; i <= 20; i++) fac *= i;
        System.out.print("20! = ");
        System.out.println(fac);        // 2432902008176640000

        double pi = 3.141592653589793;
        double r = 2.0;
        System.out.print("Kreisflaeche = ");
        System.out.println(pi * r * r); // 12.5664

        double x = 7.0 / 2.0;
        System.out.print("7.0/2.0 = ");
        System.out.println(x);          // 3.5

        // Konvertierungen
        int n = 42;
        long ln = n;                    // i2l
        double dn = n;                  // i2d
        int back = (int) (dn * 2.5);    // d2i
        System.out.println(ln + back);  // 42 + 105 = 147

        // long-Vergleich und Division
        System.out.println(b / 7L);     // 142857142857142857
        System.out.println(b > a ? 1 : 0);  // 1

        // long/double in Konkatenation
        System.out.println("b=" + b + " pi=" + pi);
    }
}
