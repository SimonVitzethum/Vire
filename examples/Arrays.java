public class Arrays {
    public static void main(String[] args) {
        // int[]: Erzeugen, Füllen, Summieren
        int[] a = new int[10];
        for (int i = 0; i < a.length; i++) {
            a[i] = i * i;
        }
        int sum = 0;
        for (int i = 0; i < a.length; i++) {
            sum += a[i];
        }
        System.out.print("sum of squares 0..9 = ");
        System.out.println(sum);          // 285

        // ref[]: Array von Objekten (RC-verwaltet)
        Box[] boxes = new Box[3];
        boxes[0] = new Box(10);
        boxes[1] = new Box(20);
        boxes[2] = new Box(30);
        int t = 0;
        for (int i = 0; i < boxes.length; i++) {
            t += boxes[i].v;
        }
        System.out.print("sum of boxes = ");
        System.out.println(t);            // 60

        // Überschreiben (altes Objekt muss released werden)
        boxes[0] = new Box(99);
        System.out.println(boxes[0].v);   // 99

        // Bounds-Check
        System.out.println(a[10]);        // ArrayIndexOutOfBoundsException
    }
}
