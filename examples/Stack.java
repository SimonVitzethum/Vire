public class Stack {
    public static void main(String[] args) {
        System.out.println(localSum());     // Point entkommt nicht → Stack
        System.out.println(escaper().x);    // Point entkommt (Return) → Heap
        int s = 0;
        for (int i = 0; i < 3; i++) {
            s += inLoop(i);                 // New in Schleife → Heap (konservativ)
        }
        System.out.println(s);
    }

    static int localSum() {
        Point p = new Point(3, 4);
        return p.x + p.y;
    }

    static Point escaper() {
        return new Point(9, 1);
    }

    static int inLoop(int i) {
        Point p = new Point(i, i);
        return p.x * p.y;
    }
}

class Point {
    int x;
    int y;

    Point(int x, int y) {
        this.x = x;
        this.y = y;
    }
}
