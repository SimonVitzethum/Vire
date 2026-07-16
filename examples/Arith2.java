public class Arith2 {
    public static void main(String[] args) {
        // abfangbare ArithmeticException
        System.out.println(safeDiv(10, 2));   // 5
        System.out.println(safeDiv(10, 0));   // -1 (gefangen)

        try {
            int x = 100 / 0;
            System.out.println("nicht erreicht " + x);
        } catch (ArithmeticException e) {
            System.out.println("division durch null gefangen");
        }

        // long division auch
        try {
            long y = 5L % 0L;
            System.out.println(y);
        } catch (RuntimeException e) {
            System.out.println("long rem gefangen");
        }

        System.out.println("weiter nach catch");
    }

    static int safeDiv(int a, int b) {
        try {
            return a / b;
        } catch (ArithmeticException e) {
            return -1;
        }
    }
}
