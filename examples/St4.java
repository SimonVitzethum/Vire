import java.util.ArrayList;
public class St4 {
    public static void main(String[] args) {
        ArrayList<String> l = new ArrayList<>();
        l.add("apfel"); l.add("bo");
        long n = l.stream().filter(s -> s.length() < 3).count();
        System.out.println("count: " + n);
    }
}
