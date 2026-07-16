import java.util.ArrayList;
public class St1 {
    public static void main(String[] args) {
        ArrayList<String> l = new ArrayList<>();
        l.add("apfel"); l.add("bo");
        l.stream().forEach(s -> System.out.println(s));
    }
}
