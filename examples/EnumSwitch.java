// switch over enum: javac generates a synthetic helper class
// (EnumSwitch$1) with a $SwitchMap int[] that maps ordinal() to dense
// case labels; its <clinit> builds the table (defensively in
// try/catch(NoSuchFieldError)). All ordinary bytecode — works
// as soon as the synthetic class is included as closed-world input.
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
