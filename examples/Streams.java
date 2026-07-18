import java.util.ArrayList;
import java.util.stream.Stream;

public class Streams {
    public static void main(String[] args) {
        ArrayList<String> l = new ArrayList<>();
        l.add("apple"); l.add("bo"); l.add("lemon"); l.add("da");

        // filter + forEach with lambdas
        System.out.println("long (>=5):");
        l.stream().filter(s -> s.length() >= 5).forEach(s -> System.out.println("  " + s));

        // map (String -> Integer, length) + forEach
        System.out.println("lengths:");
        l.stream().map(s -> s.length()).forEach(n -> System.out.println("  " + n));

        // count after filter
        long kurz = l.stream().filter(s -> s.length() < 3).count();
        System.out.println("short: " + kurz);

        // chain: filter + map + forEach with method reference
        System.out.println("filtered+mapped:");
        l.stream().filter(s -> s.length() > 2).map(String::length).forEach(n -> System.out.println("  " + n));
    }
}
