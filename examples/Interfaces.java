interface Animal {
    String sound();
    int legs();
}

interface Named {
    String name();
}

class Dog implements Animal, Named {
    public String sound() { return "woof"; }
    public int legs() { return 4; }
    public String name() { return "Dog"; }
}

class Bird implements Animal {
    public String sound() { return "tweet"; }
    public int legs() { return 2; }
}

public class Interfaces {
    public static void main(String[] args) {
        Animal a = new Dog();
        Animal b = new Bird();

        // polymorphic interface dispatch (two impls → not devirtualized)
        System.out.println(a.sound() + " " + a.legs());  // woof 4
        System.out.println(b.sound() + " " + b.legs());  // tweet 2

        describe(a);
        describe(b);

        // interface via a different interface reference
        Named n = new Dog();
        System.out.println(n.name());   // Dog
    }

    static void describe(Animal x) {
        System.out.println("goes " + x.sound() + " with " + x.legs() + " legs");
    }
}
