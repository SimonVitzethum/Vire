#include <cstdio>
#include <cstdint>
#include <vector>
int main(){ int64_t n=20000000; std::vector<int64_t> f(n,0);
  for(int64_t i=2;i<n;i++) f[i]=1;
  int64_t c=0; for(int64_t p=2;p<n;p++){ if(f[p]==1){ c++; for(int64_t k=p+p;k<n;k+=p) f[k]=0; } }
  printf("%ld\n", c); }
