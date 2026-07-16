interface Animal {
    String sound();
    int legs();
}

interface Named {
    String name();
}

class Dog implements Animal, Named {
    public String sound() { return "wuff"; }
    public int legs() { return 4; }
    public String name() { return "Hund"; }
}

class Bird implements Animal {
    public String sound() { return "piep"; }
    public int legs() { return 2; }
}

public class Interfaces {
    public static void main(String[] args) {
        Animal a = new Dog();
        Animal b = new Bird();

        // polymorpher Interface-Dispatch (zwei Impls → nicht devirtualisiert)
        System.out.println(a.sound() + " " + a.legs());  // wuff 4
        System.out.println(b.sound() + " " + b.legs());  // piep 2

        describe(a);
        describe(b);

        // Interface über eine andere Interface-Referenz
        Named n = new Dog();
        System.out.println(n.name());   // Hund
    }

    static void describe(Animal x) {
        System.out.println("macht " + x.sound() + " mit " + x.legs() + " Beinen");
    }
}
