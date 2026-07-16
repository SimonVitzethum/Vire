import java.util.ArrayList;
import java.util.stream.Stream;

public class Streams {
    public static void main(String[] args) {
        ArrayList<String> l = new ArrayList<>();
        l.add("apfel"); l.add("bo"); l.add("citrone"); l.add("da");

        // filter + forEach mit Lambdas
        System.out.println("lang (>=5):");
        l.stream().filter(s -> s.length() >= 5).forEach(s -> System.out.println("  " + s));

        // map (String -> Integer, Länge) + forEach
        System.out.println("laengen:");
        l.stream().map(s -> s.length()).forEach(n -> System.out.println("  " + n));

        // count nach filter
        long kurz = l.stream().filter(s -> s.length() < 3).count();
        System.out.println("kurze: " + kurz);

        // Kette: filter + map + forEach mit Methoden-Referenz
        System.out.println("gefiltert+gemappt:");
        l.stream().filter(s -> s.length() > 2).map(String::length).forEach(n -> System.out.println("  " + n));
    }
}
