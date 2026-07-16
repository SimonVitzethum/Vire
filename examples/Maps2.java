public class Maps2 {
    public static void main(String[] args) {
        MiniMap<String, Integer> ages = new MiniMap<>();
        ages.put("Anna", 30);
        ages.put("Bert", 25);
        System.out.println("Anna: " + ages.get("Anna"));
        ages.put("Anna", 31);
        System.out.println("Anna neu: " + ages.get("Anna"));
    }
}
