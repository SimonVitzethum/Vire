import java.util.ArrayList;

public class UseJavaUtil {
    public static void main(String[] args) {
        ArrayList<String> list = new ArrayList<>();
        list.add("echtes");
        list.add("java.util");
        list.add("ArrayList");
        for (int i = 0; i < list.size(); i++) {
            System.out.println(i + ": " + list.get(i));
        }
        System.out.println("size = " + list.size());
    }
}
