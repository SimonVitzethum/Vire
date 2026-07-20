#include <cstdio>
#include <thread>
#include <atomic>
static long sample(long base) {
    long seed = base * 2654435761L + 1, count = 0;
    for (long i = 0; i < 25000000L; i++) {
        seed = (seed * 1103515245L + 12345) % 2147483648L;
        long x = seed % 32768;
        seed = (seed * 1103515245L + 12345) % 2147483648L;
        long y = seed % 32768;
        if (x * x + y * y <= 1073676289L) count++;
    }
    return count;
}
int main() {
    std::atomic<long> hits{0};
    std::thread ts[4];
    for (long b = 0; b < 4; b++) ts[b] = std::thread([&hits, b]{ hits.fetch_add(sample(b)); });
    for (auto& t : ts) t.join();
    printf("%ld\n", hits.load());
}
