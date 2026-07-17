const PI: f64 = 3.141592653589793; const SOLAR: f64 = 4.0*PI*PI; const DAYS: f64 = 365.24;
fn advance(x:&mut[f64],y:&mut[f64],z:&mut[f64],vx:&mut[f64],vy:&mut[f64],vz:&mut[f64],m:&[f64],nb:usize,dt:f64){
    for i in 0..nb { for j in i+1..nb {
        let dx=x[i]-x[j]; let dy=y[i]-y[j]; let dz=z[i]-z[j];
        let d2=dx*dx+dy*dy+dz*dz; let mag=dt/(d2*d2.sqrt());
        let (mi,mj)=(m[i],m[j]);
        vx[i]-=dx*mj*mag; vy[i]-=dy*mj*mag; vz[i]-=dz*mj*mag;
        vx[j]+=dx*mi*mag; vy[j]+=dy*mi*mag; vz[j]+=dz*mi*mag;
    }}
    for i in 0..nb { x[i]+=dt*vx[i]; y[i]+=dt*vy[i]; z[i]+=dt*vz[i]; }
}
fn energy(x:&[f64],y:&[f64],z:&[f64],vx:&[f64],vy:&[f64],vz:&[f64],m:&[f64],nb:usize)->f64{
    let mut e=0.0; for i in 0..nb { e+=0.5*m[i]*(vx[i]*vx[i]+vy[i]*vy[i]+vz[i]*vz[i]);
        for j in i+1..nb { let dx=x[i]-x[j];let dy=y[i]-y[j];let dz=z[i]-z[j]; e-=m[i]*m[j]/(dx*dx+dy*dy+dz*dz).sqrt(); }} e
}
fn main(){
    let nb=5;
    let mut x=vec![0.0,4.841431442464721,8.343366718844633,12.894369562139131,15.379697114850917];
    let mut y=vec![0.0,-1.1603200440274284,4.124798564124305,-15.111151401698631,-25.919314609987964];
    let mut z=vec![0.0,-0.10362204447112311,-0.4035234171143214,-0.2233066787273813,0.17925877295037118];
    let mut vx=vec![0.0,0.606326392995832,0.11249649021521125,0.17048499224655595,0.09796377897625179];
    let mut vy=vec![0.0,2.81198684491626,1.0827910064415354,0.7647705393994341,0.5946989986476762];
    let mut vz=vec![0.0,-0.02521836165988763,-0.010832637401363636,-0.0035516231607529294,-0.03457701968597895];
    let m=vec![SOLAR,9.547919384243266e-4*SOLAR,2.858859806661308e-4*SOLAR,4.366244043351563e-5*SOLAR,5.151389020466116e-5*SOLAR];
    for i in 1..nb { vx[i]*=DAYS; vy[i]*=DAYS; vz[i]*=DAYS; }
    let (mut px,mut py,mut pz)=(0.0,0.0,0.0);
    for i in 0..nb { px+=vx[i]*m[i]; py+=vy[i]*m[i]; pz+=vz[i]*m[i]; }
    vx[0]=-px/SOLAR; vy[0]=-py/SOLAR; vz[0]=-pz/SOLAR;
    for _ in 0..20_000_000 { advance(&mut x,&mut y,&mut z,&mut vx,&mut vy,&mut vz,&m,nb,0.01); }
    println!("{:.9}", energy(&x,&y,&z,&vx,&vy,&vz,&m,nb));
}
