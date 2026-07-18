public class Arith2 {
    public static void main(String[] args) {
        // catchable ArithmeticException
        System.out.println(safeDiv(10, 2));   // 5
        System.out.println(safeDiv(10, 0));   // -1 (caught)

        try {
            int x = 100 / 0;
            System.out.println("not reached " + x);
        } catch (ArithmeticException e) {
            System.out.println("division by zero caught");
        }

        // long division too
        try {
            long y = 5L % 0L;
            System.out.println(y);
        } catch (RuntimeException e) {
            System.out.println("long rem caught");
        }

        System.out.println("continuing after catch");
    }

    static int safeDiv(int a, int b) {
        try {
            return a / b;
        } catch (ArithmeticException e) {
            return -1;
        }
    }
}
