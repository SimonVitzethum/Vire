public class Collections {
    public static void main(String[] args) {
        // generic list of strings (checkcast String = passthrough)
        MiniList<String> names = new MiniList<>();
        names.add("Anna");
        names.add("Bert");
        names.add("Cora");
        for (int i = 0; i < names.size(); i++) {
            System.out.println(i + ": " + names.get(i));
        }

        // growth beyond the initial capacity (4)
        MiniList<String> many = new MiniList<>();
        for (int i = 0; i < 10; i++) many.add("n" + i);
        System.out.println("size = " + many.size());     // 10
        System.out.println("last = " + many.get(9));   // n9

        // generic list of modeled objects (checkcast Box = runtime)
        MiniList<Box> boxes = new MiniList<>();
        boxes.add(new Box(10));
        boxes.add(new Box(20));
        int sum = 0;
        for (int i = 0; i < boxes.size(); i++) {
            Box b = boxes.get(i);   // checkcast Box (runtime check)
            sum += b.v;
        }
        System.out.println("sum = " + sum);   // 30

        names.set(0, "Zoe");
        System.out.println(names.get(0));        // Zoe
    }
}
