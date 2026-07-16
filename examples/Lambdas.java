interface IntOp { int apply(int x); }
interface IntBiOp { int apply(int a, int b); }
public class Lambdas {
    public static void main(String[] args) {
        // nicht-einfangendes Lambda
        IntOp dbl = x -> x * 2;
        System.out.println(dbl.apply(21));       // 42

        // einfangendes Lambda (captured c)
        int c = 100;
        IntOp addC = x -> x + c;
        System.out.println(addC.apply(5));        // 105

        // zwei Parameter
        IntBiOp add = (a, b) -> a + b;
        System.out.println(add.apply(3, 4));      // 7

        // Lambda als Argument
        System.out.println(applyTwice(dbl, 5));   // 20

        // mehrere Captures
        int base = 1000;
        int step = 10;
        IntOp f = x -> base + step * x;
        System.out.println(f.apply(3));           // 1030
    }

    static int applyTwice(IntOp op, int v) {
        return op.apply(op.apply(v));
    }
}
