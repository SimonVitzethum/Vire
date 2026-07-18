public class Exc {
    public static void main(String[] args) {
        // 1. throw + catch in the same frame, across a call
        try {
            int r = risky(5);
            System.out.println("no throw: " + r);
        } catch (RuntimeException e) {
            System.out.println("caught (5)");
        }

        try {
            int r = risky(-1);
            System.out.println("no throw: " + r);
        } catch (RuntimeException e) {
            System.out.println("caught (-1)");
        }

        // 2. propagation across two levels
        try {
            outer();
        } catch (RuntimeException e) {
            System.out.println("caught from outer");
        }

        // 3. no throw → normal path
        System.out.println("sum = " + safe(3, 4));

        System.out.println("end");
    }

    static int risky(int x) {
        if (x < 0) throw new MyException();
        return x * 2;
    }

    static void outer() {
        inner();
        System.out.println("after inner (not reached)");
    }

    static void inner() {
        throw new MyException();
    }

    static int safe(int a, int b) {
        return a + b;
    }
}

