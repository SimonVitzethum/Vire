class U { static int dbl(int x) { return x * 2; } static String tag(int x) { return "#" + x; } }
interface IntF { int apply(Integer i); }
interface StrF { String apply(Integer i); }
public class Unbox {
    public static void main(String[] args) {
        // Methoden-Referenz auf int-Methode, SAM übergibt Integer → Unboxing
        IntF f = U::dbl;
        System.out.println(f.apply(21));      // 42

        StrF g = U::tag;
        System.out.println(g.apply(7));       // #7

        // in einem Stream: map Integer -> Integer über int-Methodenref
        java.util.ArrayList<Integer> l = new java.util.ArrayList<>();
        l.add(1); l.add(2); l.add(3);
        Integer sum = 0;
        for (int i = 0; i < l.size(); i++) sum = sum + f.apply(l.get(i));
        System.out.println("summe verdoppelt = " + sum);  // 12
    }
}
