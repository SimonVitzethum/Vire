public class Refl {
    public static void main(String[] args) throws Exception {
        // "Dynamic" class loading, statically resolved (closed world):
        Class<?> c = Class.forName("Widget");
        System.out.println(c.getName());

        Widget w = (Widget) c.newInstance();
        w.id = 42;
        System.out.println(w.id);

        // Class objects are singletons → identity as in Java:
        System.out.println(c == Widget.class ? 1 : 0);
        System.out.println(c == Refl.class ? 1 : 0);
    }
}

class Widget {
    int id;

    Widget() {
        this.id = 7;
    }
}
