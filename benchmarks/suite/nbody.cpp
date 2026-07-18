#include <cstdio>
#include <vector>
int main(){ int n=2000; std::vector<double> px(n),py(n),vx(n,0),vy(n,0);
 for(int i=0;i<n;i++){px[i]=i%100;py[i]=i%50;}
 for(int s=0;s<20;s++){ for(int a=0;a<n;a++){double fx=0,fy=0;for(int b=0;b<n;b++){double dx=px[b]-px[a],dy=py[b]-py[a],d=dx*dx+dy*dy+1;fx+=dx/d;fy+=dy/d;}vx[a]+=fx*0.01;vy[a]+=fy*0.01;} for(int m=0;m<n;m++){px[m]+=vx[m];py[m]+=vy[m];} }
 double t=0;for(int i=0;i<n;i++)t+=px[i]; printf("%g\n",t); }
