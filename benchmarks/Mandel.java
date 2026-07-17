public class Mandel {
    public static void main(String[] args) {
        int n = 4000; long sum = 0;
        for (int py = 0; py < n; py++) {
            double y0 = (py * 2.0 / n) - 1.0;
            for (int px = 0; px < n; px++) {
                double x0 = (px * 2.5 / n) - 2.0;
                double x = 0, y = 0; int it = 0;
                while (x*x + y*y <= 4.0 && it < 100) {
                    double xt = x*x - y*y + x0;
                    y = 2*x*y + y0; x = xt; it++;
                }
                sum += it;
            }
        }
        System.out.println(sum);
    }
}
