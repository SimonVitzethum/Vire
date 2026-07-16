package java.util;

// Einfach verkettete Liste; implementiert List (add/get/size/iterator).
public class LinkedList<E> implements List<E> {
    LinkedListNode<E> head;   // package-private für Iterator
    private LinkedListNode<E> tail;
    private int count;

    public LinkedList() {
        head = null;
        tail = null;
        count = 0;
    }

    public boolean add(E e) {
        LinkedListNode<E> n = new LinkedListNode<E>(e);
        if (tail == null) {
            head = n;
            tail = n;
        } else {
            tail.next = n;
            tail = n;
        }
        count = count + 1;
        return true;
    }

    public E get(int i) {
        LinkedListNode<E> n = head;
        int k = 0;
        while (k < i) {
            n = n.next;
            k = k + 1;
        }
        return n.value;
    }

    public int size() {
        return count;
    }

    public Iterator<E> iterator() {
        return new LinkedListIterator<E>(head);
    }
}
