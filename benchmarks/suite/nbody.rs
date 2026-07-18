fn main(){ let n=2000usize; let mut px=vec![0f64;n]; let mut py=vec![0f64;n]; let mut vx=vec![0f64;n]; let mut vy=vec![0f64;n];
 for i in 0..n { px[i]=(i%100) as f64; py[i]=(i%50) as f64; }
 for _ in 0..20 { for a in 0..n { let mut fx=0f64; let mut fy=0f64; for b in 0..n { let dx=px[b]-px[a]; let dy=py[b]-py[a]; let d=dx*dx+dy*dy+1.0; fx+=dx/d; fy+=dy/d; } vx[a]+=fx*0.01; vy[a]+=fy*0.01; } for m in 0..n { px[m]+=vx[m]; py[m]+=vy[m]; } }
 let t:f64=px.iter().sum(); println!("{}",t); }
