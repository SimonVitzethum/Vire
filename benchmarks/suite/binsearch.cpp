#include <cstdio>
#include <cstdint>
#include <vector>
int main(){ int64_t n=1000000; std::vector<int64_t> a(n); for(int64_t i=0;i<n;i++)a[i]=i*2; int64_t found=0; for(int64_t q=0;q<10000000;q++){ int64_t key=(q*7)%(n*2),lo=0,hi=n-1; while(lo<=hi){int64_t mid=(lo+hi)/2; if(a[mid]==key){found++;lo=hi+1;} else if(a[mid]<key)lo=mid+1; else hi=mid-1;} } printf("%ld\n",found); }
