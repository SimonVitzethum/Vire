#include <cstdio>
#include <vector>
int main(){ int n=256; std::vector<double> a(n*n),b(n*n),c(n*n);
 for(int i=0;i<n*n;i++){a[i]=i%7;b[i]=i%5;}
 for(int r=0;r<n;r++)for(int col=0;col<n;col++){double s=0;for(int k=0;k<n;k++)s+=a[r*n+k]*b[k*n+col];c[r*n+col]=s;}
 double t=0; for(int i=0;i<n*n;i++)t+=c[i]; printf("%g\n",t); }
