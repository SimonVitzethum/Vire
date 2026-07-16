package java.util;

class LinkedListIterator<E> implements Iterator<E> {
    private LinkedListNode<E> cur;

    LinkedListIterator(LinkedListNode<E> h) {
        cur = h;
    }

    public boolean hasNext() {
        return cur != null;
    }

    public E next() {
        E v = cur.value;
        cur = cur.next;
        return v;
    }
}
