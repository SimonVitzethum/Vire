import java.util.ArrayList;
public class St5 {
    public static void main(String[] args) {
        ArrayList<String> l = new ArrayList<>();
        l.add("apfel"); l.add("bo");
        l.stream().filter(s -> s.length() > 2).map(String::length).forEach(n -> System.out.println(n));
    }
}
