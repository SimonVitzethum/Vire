public class Finally {
    public static void main(String[] args) {
        System.out.println(withFinally(false));  // finally, then 1
        System.out.println(withFinally(true));   // finally, then 2 (caught)
        cleanup();
    }

    static int withFinally(boolean fail) {
        try {
            if (fail) throw new MyException();
            return 1;
        } catch (RuntimeException e) {
            return 2;
        } finally {
            System.out.println("finally ran");
        }
    }

    static void cleanup() {
        try {
            System.out.println("try");
        } finally {
            System.out.println("cleanup");
        }
    }
}
