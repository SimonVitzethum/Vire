public class Switch {
    static String day(int d) {
        switch (d) {
            case 1: return "Mon";
            case 2: return "Tue";
            case 3: return "Wed";
            case 6:
            case 7: return "weekend";
            default: return "?";
        }
    }
    static int classify(int x) {
        // lookupswitch (widely spaced values)
        switch (x) {
            case 0: return 100;
            case 1000: return 200;
            case 1000000: return 300;
            default: return -1;
        }
    }
    public static void main(String[] args) {
        for (int i = 1; i <= 7; i++) System.out.println(i + ": " + day(i));
        System.out.println(classify(1000));      // 200
        System.out.println(classify(1000000));   // 300
        System.out.println(classify(5));         // -1

        // String-switch (hashCode + equals)
        String[] cmds = {"start", "stop", "x"};
        for (int i = 0; i < cmds.length; i++) {
            String r;
            switch (cmds[i]) {
                case "start": r = "started"; break;
                case "stop": r = "stopped"; break;
                default: r = "unknown";
            }
            System.out.println(cmds[i] + " -> " + r);
        }
    }
}
