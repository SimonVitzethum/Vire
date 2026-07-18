public class Shapes {
    public static void main(String[] args) {
        // Both subclasses instantiated → site stays polymorphic (vtable),
        // RTA cannot devirtualize.
        Shape a = new Circle(5);
        Shape b = new Rect(3, 4);
        System.out.print("circle.area() = ");
        System.out.println(a.area());
        System.out.print("rect.area()   = ");
        System.out.println(b.area());
        System.out.print("describe(a)   = ");
        System.out.println(describe(a));
        System.out.print("describe(b)   = ");
        System.out.println(describe(b));
        // Inherited method (Shape.scaledArea, no override) → monomorphic.
        System.out.print("a.scaledArea(2) = ");
        System.out.println(a.scaledArea(2));
        // Null comparisons:
        Shape n = null;
        System.out.println(n == null ? 1 : 0);
    }

    static int describe(Shape s) {
        return s.area() + s.kind();
    }
}

abstract class Shape {
    abstract int area();
    abstract int kind();

    int scaledArea(int f) {
        return area() * f;
    }
}

class Circle extends Shape {
    int r;

    Circle(int r) {
        this.r = r;
    }

    int area() {
        return 3 * r * r; // int pi :)
    }

    int kind() {
        return 1;
    }
}

class Rect extends Shape {
    int w;
    int h;

    Rect(int w, int h) {
        this.w = w;
        this.h = h;
    }

    int area() {
        return w * h;
    }

    int kind() {
        return 2;
    }
}
