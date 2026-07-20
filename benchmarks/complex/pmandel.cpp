#include <cstdio>
#include <thread>
#include <atomic>
static long band(long base) {
    long sum = 0;
    for (long y = base * 500; y < base * 500 + 500; y++)
        for (long px = 0; px < 2000; px++) {
            double cr = (double)px * 3.0 / 2000.0 - 2.0;
            double ci = (double)y * 3.0 / 2000.0 - 1.5;
            double zr = 0.0, zi = 0.0; long it = 0;
            for (int i = 0; i < 200; i++) {
                double zr2 = zr * zr, zi2 = zi * zi;
                if (zr2 + zi2 > 4.0) break;
                double nzr = zr2 - zi2 + cr;
                zi = 2.0 * zr * zi + ci;
                zr = nzr; it++;
            }
            sum += it;
        }
    return sum;
}
int main() {
    std::atomic<long> total{0};
    std::thread ts[4];
    for (long b = 0; b < 4; b++) ts[b] = std::thread([&total, b]{ total.fetch_add(band(b)); });
    for (auto& t : ts) t.join();
    printf("%ld\n", total.load());
}
