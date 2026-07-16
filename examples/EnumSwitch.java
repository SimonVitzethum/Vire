// switch über enum: javac erzeugt eine synthetische Hilfsklasse
// (EnumSwitch$1) mit einem $SwitchMap-int[], das ordinal() auf dichte
// case-Labels abbildet; deren <clinit> baut die Tabelle (defensiv in
// try/catch(NoSuchFieldError)). Alles gewöhnliches Bytecode — funktioniert,
// sobald die synthetische Klasse als Closed-World-Input dabei ist.
enum Dir { N, E, S, W }

public class EnumSwitch {
    static String turn(Dir d) {
        switch (d) {
            case N: return "north";
            case E: return "east";
            case W: return "west";
            default: return "other";
        }
    }

    public static void main(String[] args) {
        System.out.println(turn(Dir.N));
        System.out.println(turn(Dir.E));
        System.out.println(turn(Dir.S));
        System.out.println(turn(Dir.W));
    }
}
