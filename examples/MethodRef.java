interface IntBiOp { int apply(int a, int b); }
interface StrLen { int len(String s); }
interface Maker { Box make(int v); }
class Box { int v; Box(int v) { this.v = v; } int get() { return v; } }
class MathU { static int max(int a, int b) { return a > b ? a : b; } }

public class MethodRef {
    public static void main(String[] args) {
        // static method reference
        IntBiOp max = MathU::max;
        System.out.println(max.apply(3, 7));      // 7

        // unbound instance method reference (receiver = argument)
        StrLen len = String::length;
        System.out.println(len.len("hello"));     // 5

        // constructor reference
        Maker m = Box::new;
        Box b = m.make(42);
        System.out.println(b.get());              // 42
    }
}
