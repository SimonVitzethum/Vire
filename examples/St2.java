import java.util.ArrayList;
public class St2 {
    public static void main(String[] args) {
        ArrayList<String> l = new ArrayList<>();
        l.add("apfel"); l.add("bo");
        l.stream().filter(s -> s.length() >= 3).forEach(s -> System.out.println(s));
    }
}
