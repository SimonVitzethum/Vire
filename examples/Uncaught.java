public class Uncaught {
    public static void main(String[] args) {
        System.out.println("vor dem Wurf");
        deep();
        System.out.println("nach dem Wurf (nicht erreicht)");
    }
    static void deep() { throw new MyException(); }
}
