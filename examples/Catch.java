class ErrorA extends RuntimeException {}
class ErrorB extends RuntimeException {}
class ErrorC extends ErrorA {}   // Subklasse von A

public class Catch {
    public static void main(String[] args) {
        // typspezifische Diskriminierung
        test(1);   // wirft A → fängt A
        test(2);   // wirft B → fängt B
        test(3);   // wirft C (extends A) → fängt A (Subklasse!)
        test(4);   // wirft nichts
    }

    static void test(int which) {
        try {
            throwIt(which);
            System.out.println(which + ": kein Wurf");
        } catch (ErrorB e) {
            System.out.println(which + ": fing B");
        } catch (ErrorA e) {
            System.out.println(which + ": fing A");
        }
    }

    static void throwIt(int which) {
        if (which == 1) throw new ErrorA();
        if (which == 2) throw new ErrorB();
        if (which == 3) throw new ErrorC();
    }
}
