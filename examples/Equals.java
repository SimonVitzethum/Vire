class Point {
    int x, y;
    Point(int x, int y) { this.x = x; this.y = y; }
    public boolean equals(Object o) {
        if (!(o instanceof Point)) return false;
        Point p = (Point) o;
        return x == p.x && y == p.y;
    }
    public int hashCode() { return x * 31 + y; }
}

public class Equals {
    public static void main(String[] args) {
        // equals via Object reference to String → jrt_str_equals
        Object a = "hello";
        Object b = "hello";
        Object c = "world";
        System.out.println(a.equals(b) ? 1 : 0);   // 1
        System.out.println(a.equals(c) ? 1 : 0);   // 0

        // equals on user class with override
        Object p1 = new Point(1, 2);
        Object p2 = new Point(1, 2);
        Object p3 = new Point(3, 4);
        System.out.println(p1.equals(p2) ? 1 : 0);  // 1
        System.out.println(p1.equals(p3) ? 1 : 0);  // 0

        // equals on user class without override (identity)
        Object o1 = new Plain();
        Object o2 = new Plain();
        System.out.println(o1.equals(o1) ? 1 : 0);  // 1
        System.out.println(o1.equals(o2) ? 1 : 0);  // 0

        // hashCode consistent for equal strings
        System.out.println(a.hashCode() == b.hashCode() ? 1 : 0);  // 1
    }
}

class Plain {}
