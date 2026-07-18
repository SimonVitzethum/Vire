#include <math.h>
double geo_hypot(double a, double b){ return hypot(a,b); }
long geo_isqrt(long n){ long r=0; while((r+1)*(r+1)<=n) r++; return r; }
