public class Exc {
    public static void main(String[] args) {
        // 1. throw + catch im selben Rahmen, über Aufruf
        try {
            int r = risky(5);
            System.out.println("kein wurf: " + r);
        } catch (RuntimeException e) {
            System.out.println("gefangen (5)");
        }

        try {
            int r = risky(-1);
            System.out.println("kein wurf: " + r);
        } catch (RuntimeException e) {
            System.out.println("gefangen (-1)");
        }

        // 2. Propagation über zwei Ebenen
        try {
            outer();
        } catch (RuntimeException e) {
            System.out.println("gefangen aus outer");
        }

        // 3. kein Wurf → normaler Pfad
        System.out.println("summe = " + safe(3, 4));

        System.out.println("ende");
    }

    static int risky(int x) {
        if (x < 0) throw new MyException();
        return x * 2;
    }

    static void outer() {
        inner();
        System.out.println("nach inner (nicht erreicht)");
    }

    static void inner() {
        throw new MyException();
    }

    static int safe(int a, int b) {
        return a + b;
    }
}

class MyException extends RuntimeException {
}
