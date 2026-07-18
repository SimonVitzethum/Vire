#include <cstdio>
#include <cstdint>
#include <vector>
void qsort2(int64_t*a,int64_t lo,int64_t hi){ if(lo<hi){ int64_t p=a[hi],i=lo-1; for(int64_t j=lo;j<hi;j++) if(a[j]<p){i++;int64_t t=a[i];a[i]=a[j];a[j]=t;} int64_t t=a[i+1];a[i+1]=a[hi];a[hi]=t; qsort2(a,lo,i); qsort2(a,i+2,hi);} }
int main(){ int64_t n=2000000; std::vector<int64_t> a(n); int64_t seed=987654321; for(int64_t i=0;i<n;i++){seed=(seed*1103515245+12345)%2147483648;a[i]=seed%1000000;} qsort2(a.data(),0,n-1); printf("%ld\n",a[0]+a[n/2]+a[n-1]); }
