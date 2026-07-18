#include <vector>
#include <algorithm>
#include <cstdint>
// C++ internals (std::vector/sort), C ABI on the outside.
extern "C" int64_t cpp_median_of_squares(int64_t n) {
    std::vector<int64_t> v;
    for (int64_t i = 0; i < n; i++) v.push_back((i * 7 + 3) % n);
    std::sort(v.begin(), v.end());
    int64_t m = v[v.size() / 2];
    return m * m;
}
