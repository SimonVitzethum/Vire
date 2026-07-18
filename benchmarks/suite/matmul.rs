fn main(){ let n=256usize; let mut a=vec![0f64;n*n]; let mut b=vec![0f64;n*n]; let mut c=vec![0f64;n*n];
 for i in 0..n*n { a[i]=(i%7) as f64; b[i]=(i%5) as f64; }
 for r in 0..n { for col in 0..n { let mut s=0f64; for k in 0..n { s+=a[r*n+k]*b[k*n+col]; } c[r*n+col]=s; } }
 let t:f64=c.iter().sum(); println!("{}",t); }
