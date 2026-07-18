class ErrorA extends RuntimeException {}
class ErrorB extends RuntimeException {}
class ErrorC extends ErrorA {}   // subclass of A

public class Catch {
    public static void main(String[] args) {
        // type-specific discrimination
        test(1);   // throws A → catches A
        test(2);   // throws B → catches B
        test(3);   // throws C (extends A) → catches A (subclass!)
        test(4);   // throws nothing
    }

    static void test(int which) {
        try {
            throwIt(which);
            System.out.println(which + ": no throw");
        } catch (ErrorB e) {
            System.out.println(which + ": caught B");
        } catch (ErrorA e) {
            System.out.println(which + ": caught A");
        }
    }

    static void throwIt(int which) {
        if (which == 1) throw new ErrorA();
        if (which == 2) throw new ErrorB();
        if (which == 3) throw new ErrorC();
    }
}
