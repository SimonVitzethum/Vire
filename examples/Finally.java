public class Finally {
    public static void main(String[] args) {
        System.out.println(withFinally(false));  // finally, dann 1
        System.out.println(withFinally(true));   // finally, dann 2 (gefangen)
        cleanup();
    }

    static int withFinally(boolean fail) {
        try {
            if (fail) throw new MyException();
            return 1;
        } catch (RuntimeException e) {
            return 2;
        } finally {
            System.out.println("finally lief");
        }
    }

    static void cleanup() {
        try {
            System.out.println("versuch");
        } finally {
            System.out.println("aufraeumen");
        }
    }
}
