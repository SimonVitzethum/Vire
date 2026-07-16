public class Collections {
    public static void main(String[] args) {
        // generische Liste von Strings (checkcast String = passthrough)
        MiniList<String> names = new MiniList<>();
        names.add("Anna");
        names.add("Bert");
        names.add("Cora");
        for (int i = 0; i < names.size(); i++) {
            System.out.println(i + ": " + names.get(i));
        }

        // Wachstum über die Anfangskapazität (4) hinaus
        MiniList<String> many = new MiniList<>();
        for (int i = 0; i < 10; i++) many.add("n" + i);
        System.out.println("size = " + many.size());     // 10
        System.out.println("letztes = " + many.get(9));   // n9

        // generische Liste modellierter Objekte (checkcast Box = Laufzeit)
        MiniList<Box> boxes = new MiniList<>();
        boxes.add(new Box(10));
        boxes.add(new Box(20));
        int sum = 0;
        for (int i = 0; i < boxes.size(); i++) {
            Box b = boxes.get(i);   // checkcast Box (Laufzeit-Prüfung)
            sum += b.v;
        }
        System.out.println("summe = " + sum);   // 30

        names.set(0, "Zoe");
        System.out.println(names.get(0));        // Zoe
    }
}
