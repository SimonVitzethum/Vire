public class Matmul {
    public static void main(String[] args) {
        int n = 512;
        double[] A = new double[n*n], B = new double[n*n], C = new double[n*n];
        for (int i = 0; i < n*n; i++) { A[i] = i % 7; B[i] = i % 5; }
        for (int i = 0; i < n; i++)
            for (int k = 0; k < n; k++) {
                double aik = A[i*n + k];
                int ci = i*n, bk = k*n;
                for (int j = 0; j < n; j++) C[ci + j] += aik * B[bk + j];
            }
        double s = 0; for (int i = 0; i < n; i++) s += C[i*n + i];
        System.out.println((long) s);
    }
}
