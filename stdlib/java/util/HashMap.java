package java.util;

// Hash-Map mit open addressing; nutzt hashCode()/equals() der Schlüssel
// (virtueller Dispatch über Object).
public class HashMap<K, V> implements Map<K, V> {
    private Object[] keys;
    private Object[] vals;
    private int count;
    private int cap;

    public HashMap() {
        cap = 16;
        keys = new Object[cap];
        vals = new Object[cap];
        count = 0;
    }

    private int slotFor(Object k) {
        int h = k.hashCode() & 0x7fffffff;
        int i = h % cap;
        while (keys[i] != null && !keys[i].equals(k)) {
            i = (i + 1) % cap;
        }
        return i;
    }

    public V put(K k, V v) {
        if (count * 2 >= cap) resize();
        int i = slotFor(k);
        Object old = vals[i];
        if (keys[i] == null) count = count + 1;
        keys[i] = k;
        vals[i] = v;
        return (V) old;
    }

    public V get(Object k) {
        int i = slotFor(k);
        if (keys[i] == null) return null;
        return (V) vals[i];
    }

    public boolean containsKey(Object k) {
        return keys[slotFor(k)] != null;
    }

    public int size() {
        return count;
    }

    private void resize() {
        Object[] ok = keys;
        Object[] ov = vals;
        cap = cap * 2;
        keys = new Object[cap];
        vals = new Object[cap];
        count = 0;
        for (int j = 0; j < ok.length; j++) {
            if (ok[j] != null) {
                int i = slotFor(ok[j]);
                keys[i] = ok[j];
                vals[i] = ov[j];
                count = count + 1;
            }
        }
    }
}
