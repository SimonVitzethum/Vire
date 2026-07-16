package java.util;

// Minimaler Stub im java.util-Paket, kompiliert über --patch-module.
// fastjavac nutzt diese .class; javac prüft Nutzercode gegen das echte JDK.
public class ArrayList<E> {
    private Object[] elems;
    private int count;

    public ArrayList() {
        elems = new Object[4];
        count = 0;
    }

    public boolean add(E e) {
        if (count == elems.length) grow();
        elems[count] = e;
        count = count + 1;
        return true;
    }

    public E get(int i) {
        return (E) elems[i];
    }

    public int size() {
        return count;
    }

    private void grow() {
        Object[] bigger = new Object[elems.length * 2];
        for (int i = 0; i < count; i++) bigger[i] = elems[i];
        elems = bigger;
    }
}
