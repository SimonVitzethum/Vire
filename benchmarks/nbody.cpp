#include <cstdio>
#include <cmath>
static const double PI=3.141592653589793, SOLAR=4*PI*PI, DAYS=365.24;
double x[5],y[5],z[5],vx[5],vy[5],vz[5],mass[5];
void advance(int nb,double dt){
    for(int i=0;i<nb;i++)for(int j=i+1;j<nb;j++){
        double dx=x[i]-x[j],dy=y[i]-y[j],dz=z[i]-z[j];
        double d2=dx*dx+dy*dy+dz*dz, mag=dt/(d2*sqrt(d2));
        double mi=mass[i],mj=mass[j];
        vx[i]-=dx*mj*mag;vy[i]-=dy*mj*mag;vz[i]-=dz*mj*mag;
        vx[j]+=dx*mi*mag;vy[j]+=dy*mi*mag;vz[j]+=dz*mi*mag;
    }
    for(int i=0;i<nb;i++){x[i]+=dt*vx[i];y[i]+=dt*vy[i];z[i]+=dt*vz[i];}
}
double energy(int nb){double e=0;for(int i=0;i<nb;i++){e+=0.5*mass[i]*(vx[i]*vx[i]+vy[i]*vy[i]+vz[i]*vz[i]);
    for(int j=i+1;j<nb;j++){double dx=x[i]-x[j],dy=y[i]-y[j],dz=z[i]-z[j];e-=mass[i]*mass[j]/sqrt(dx*dx+dy*dy+dz*dz);}}return e;}
int main(){
    int nb=5;
    double X[]={0,4.841431442464721,8.343366718844633,12.894369562139131,15.379697114850917};
    double Y[]={0,-1.1603200440274284,4.124798564124305,-15.111151401698631,-25.919314609987964};
    double Z[]={0,-0.10362204447112311,-0.4035234171143214,-0.2233066787273813,0.17925877295037118};
    double VX[]={0,0.606326392995832,0.11249649021521125,0.17048499224655595,0.09796377897625179};
    double VY[]={0,2.81198684491626,1.0827910064415354,0.7647705393994341,0.5946989986476762};
    double VZ[]={0,-0.02521836165988763,-0.010832637401363636,-0.0035516231607529294,-0.03457701968597895};
    double M[]={SOLAR,9.547919384243266e-4*SOLAR,2.858859806661308e-4*SOLAR,4.366244043351563e-5*SOLAR,5.151389020466116e-5*SOLAR};
    for(int i=0;i<5;i++){x[i]=X[i];y[i]=Y[i];z[i]=Z[i];vx[i]=VX[i];vy[i]=VY[i];vz[i]=VZ[i];mass[i]=M[i];}
    for(int i=1;i<nb;i++){vx[i]*=DAYS;vy[i]*=DAYS;vz[i]*=DAYS;}
    double px=0,py=0,pz=0;for(int i=0;i<nb;i++){px+=vx[i]*mass[i];py+=vy[i]*mass[i];pz+=vz[i]*mass[i];}
    vx[0]=-px/SOLAR;vy[0]=-py/SOLAR;vz[0]=-pz/SOLAR;
    for(int i=0;i<20000000;i++)advance(nb,0.01);
    printf("%.9f\n",energy(nb));
}
