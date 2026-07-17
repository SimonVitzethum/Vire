public class Intrinsics {
    public static void main(String[] args) {
        int[] a = {1,2,3,4,5};
        int[] b = new int[5];
        System.arraycopy(a, 1, b, 0, 3);
        System.out.println(b[0] + b[1] + b[2]);       // 9
        System.out.println(Integer.parseInt("-42"));    // -42
        System.out.println(Math.max(7, 3) + Math.min(7, 3)); // 10
        System.out.println(Math.abs(-15));              // 15
        System.out.println((int) Math.sqrt(144.0));     // 12
        String[] s = {"x","y","z"};
        String[] t = new String[3];
        System.arraycopy(s, 0, t, 0, 3);
        System.out.println(t[2]);                        // z
    }
}
