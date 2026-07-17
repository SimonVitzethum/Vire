class Animal {}
class Dog extends Animal {}
public class Refl2 {
    public static void main(String[] args) {
        Object o = new Dog();
        System.out.println(o.getClass().getName());        // Dog
        System.out.println(o.getClass().getSimpleName());  // Dog
        Animal a = new Animal();
        System.out.println(a.getClass().getName());        // Animal
        System.out.println(o.getClass() == a.getClass() ? "same" : "diff"); // diff
        String s = "hi";
        System.out.println(s.getClass().getName());        // java.lang.String
    }
}
