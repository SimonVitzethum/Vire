// try-with-resources: javac entzuckert es zu try/catch(Throwable) + close()
// + addSuppressed + athrow. Wir prüfen, dass Ressourcen in umgekehrter
// Reihenfolge geschlossen werden — sowohl normal als auch bei Exception.
class Res implements AutoCloseable {
    String n;
    Res(String n) { this.n = n; System.out.println("open " + n); }
    public void close() { System.out.println("close " + n); }
    void use() { System.out.println("use " + n); }
}

public class Twr {
    static void normal() {
        try (Res a = new Res("A"); Res b = new Res("B")) {
            a.use();
            b.use();
        }
    }

    static void withThrow() {
        try (Res a = new Res("A"); Res b = new Res("B")) {
            a.use();
            throw new MyException();
        } catch (RuntimeException e) {
            System.out.println("caught");
        }
    }

    public static void main(String[] args) {
        normal();
        withThrow();
        System.out.println("done");
    }
}
