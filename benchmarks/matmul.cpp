#include <cstdio>
#include <cstdint>
#include <vector>
int main() {
    int n = 512;
    std::vector<double> A(n*n), B(n*n), C(n*n, 0.0);
    for (int i = 0; i < n*n; i++) { A[i] = i % 7; B[i] = i % 5; }
    for (int i = 0; i < n; i++)
        for (int k = 0; k < n; k++) {
            double aik = A[i*n + k];
            for (int j = 0; j < n; j++) C[i*n + j] += aik * B[k*n + j];
        }
    double s = 0; for (int i = 0; i < n; i++) s += C[i*n + i];
    printf("%lld\n", (long long) s);
}
