public class Arr2 {
    public static void main(String[] args) {
        Box[] boxes = new Box[100];
        for (int i = 0; i < boxes.length; i++) {
            boxes[i] = new Box(i);
        }
        // overwrite every other one → old ones must be released
        for (int i = 0; i < boxes.length; i += 2) {
            boxes[i] = new Box(i * 10);
        }
        int t = 0;
        for (int i = 0; i < boxes.length; i++) {
            t += boxes[i].v;
        }
        System.out.println(t);   // odd i: i; even i: i*10
    }
}
