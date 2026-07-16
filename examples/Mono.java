public class Mono {
    public static void main(String[] args) {
        Shape s = new Circle(4);
        System.out.println(s.area());       // einziges RTA-Ziel: Circle.area
        Shape n = null;
        System.out.println(n.area());       // NullPointerException
    }
}
