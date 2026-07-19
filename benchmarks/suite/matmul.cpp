#include <cstdio>
#include <vector>
int main(){ int n=256; std::vector<double> a(n*n),b(n*n),c(n*n);
 for(int i=0;i<n*n;i++){a[i]=i%7;b[i]=i%5;}
 // cache-friendly ikj order: inner loop is a unit-stride SAXPY, vectorizes.
 for(int r=0;r<n;r++)for(int k=0;k<n;k++){double aik=a[r*n+k];int ci=r*n,bk=k*n;for(int j=0;j<n;j++)c[ci+j]+=aik*b[bk+j];}
 double t=0; for(int i=0;i<n*n;i++)t+=c[i]; printf("%g\n",t); }
