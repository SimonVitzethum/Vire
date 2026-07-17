public class Sync {
    static int counter = 0;
    static synchronized void inc() { counter++; }
    public static void main(String[] args) {
        Object lock = new Sync();
        synchronized (lock) { counter += 5; }
        inc();
        System.out.println(counter);
    }
}
