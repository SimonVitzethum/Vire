package java.util;

// Hash-Set mit open addressing über hashCode()/equals().
public class HashSet<E> implements Set<E> {
    Object[] slots;   // package-private für den Iterator
    int count;
    private int cap;

    public HashSet() {
        cap = 16;
        slots = new Object[cap];
        count = 0;
    }

    private int slotFor(Object e) {
        int h = e.hashCode() & 0x7fffffff;
        int i = h % cap;
        while (slots[i] != null && !slots[i].equals(e)) {
            i = (i + 1) % cap;
        }
        return i;
    }

    public boolean add(E e) {
        if (count * 2 >= cap) resize();
        int i = slotFor(e);
        if (slots[i] != null) return false;
        slots[i] = e;
        count = count + 1;
        return true;
    }

    public boolean contains(Object o) {
        return slots[slotFor(o)] != null;
    }

    public int size() {
        return count;
    }

    public Iterator<E> iterator() {
        return new HashSetIterator<E>(this);
    }

    private void resize() {
        Object[] old = slots;
        cap = cap * 2;
        slots = new Object[cap];
        count = 0;
        for (int j = 0; j < old.length; j++) {
            if (old[j] != null) {
                slots[slotFor(old[j])] = old[j];
                count = count + 1;
            }
        }
    }
}
