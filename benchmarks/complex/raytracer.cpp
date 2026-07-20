#include <cstdio>
#include <cmath>
int main() {
    long w=2400, h=2400;
    double scx[4]={0.0,2.0,-2.0,0.0}, scy[4]={0.0,0.0,1.0,-1.0}, scz[4]={5.0,6.0,5.5,4.0}, sr[4]={1.0,1.2,0.8,0.6};
    double lx=5.0, ly=5.0, lz=-2.0; long total=0;
    for (long py=0; py<h; py++) for (long px=0; px<w; px++) {
        double ddx=(double)px/(double)w-0.5, ddy=(double)py/(double)h-0.5, ddz=1.0;
        double rl=sqrt(ddx*ddx+ddy*ddy+ddz*ddz);
        double dx=ddx/rl, dy=ddy/rl, dz=ddz/rl;
        double best=1000000.0; long hit=-1;
        for (int s=0;s<4;s++) {
            double ocx=-scx[s], ocy=-scy[s], ocz=-scz[s];
            double b=ocx*dx+ocy*dy+ocz*dz, c=ocx*ocx+ocy*ocy+ocz*ocz-sr[s]*sr[s], disc=b*b-c;
            if (disc>0.0) { double t=-b-sqrt(disc); if (t>0.001 && t<best) { best=t; hit=s; } }
        }
        long bright=0;
        if (hit>=0) {
            int hs=hit;
            double hx=dx*best, hy=dy*best, hz=dz*best;
            double nx=hx-scx[hs], ny=hy-scy[hs], nz=hz-scz[hs];
            double nl=sqrt(nx*nx+ny*ny+nz*nz); nx/=nl; ny/=nl; nz/=nl;
            double ldx=lx-hx, ldy=ly-hy, ldz=lz-hz;
            double ll=sqrt(ldx*ldx+ldy*ldy+ldz*ldz); ldx/=ll; ldy/=ll; ldz/=ll;
            double diff=nx*ldx+ny*ldy+nz*ldz; if (diff<0.0) diff=0.0;
            bright=(long)(diff*255.0); if (bright>255) bright=255;
        }
        total += bright;
    }
    printf("%ld\n", total);
}
