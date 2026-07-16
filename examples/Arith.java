public class Arith {
    static int fib(int n) {
        if (n < 2) return n;
        return fib(n - 1) + fib(n - 2);
    }

    static int gcd(int a, int b) {
        while (b != 0) {
            int t = a % b;
            a = b;
            b = t;
        }
        return a;
    }

    public static void main(String[] args) {
        System.out.print("fib(20) = ");
        System.out.println(fib(20));
        System.out.print("gcd(1071, 462) = ");
        System.out.println(gcd(1071, 462));

        int sum = 0;
        for (int i = 1; i <= 100; i++) {
            sum += i;
        }
        System.out.print("sum(1..100) = ");
        System.out.println(sum);

        System.out.println(Integer.MIN_VALUE / -1); // definiert: MIN_VALUE
        System.out.println((1 << 35));              // Shift maskiert: == 1<<3
        System.out.println(7 / 0);                  // ArithmeticException
    }
}
