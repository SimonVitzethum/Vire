#include <cstdio>
#include <cstdint>
int main(){ int64_t seed=12345,inside=0; for(int64_t i=0;i<20000000;i++){ seed=(seed*1103515245+12345)%2147483648; int64_t x=seed%10000; seed=(seed*1103515245+12345)%2147483648; int64_t y=seed%10000; if(x*x+y*y<100000000)inside++; } printf("%ld\n",inside); }
