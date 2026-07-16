public class Statics {
    static int counter = 0;
    static final int LIMIT = 100;       // ConstantValue
    static String label = "init";       // <clinit>
    static int computed;                 // <clinit>

    static {
        computed = LIMIT * 2 + 5;         // 205
        label = "ready";
    }

    static void inc() { counter++; }

    public static void main(String[] args) {
        System.out.println(LIMIT);       // 100
        System.out.println(computed);    // 205
        System.out.println(label);       // ready

        for (int i = 0; i < 5; i++) inc();
        System.out.println(counter);     // 5

        Statics.counter = 42;
        System.out.println(counter);     // 42

        // statisches Ref-Feld überschreiben (RC)
        label = "x=" + counter;
        System.out.println(label);       // x=42
    }
}
