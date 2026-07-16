interface IntOp { int apply(int x); }
public class Lam {
    public static void main(String[] args) {
        IntOp f = x -> x * 2;
        System.out.println(f.apply(21));
    }
}
