fn main(){ let n=256usize; let mut a=vec![0f64;n*n]; let mut b=vec![0f64;n*n]; let mut c=vec![0f64;n*n];
 for i in 0..n*n { a[i]=(i%7) as f64; b[i]=(i%5) as f64; }
 // cache-friendly ikj order: inner loop is a unit-stride SAXPY, vectorizes.
 for r in 0..n { for k in 0..n { let aik=a[r*n+k]; let (ci,bk)=(r*n,k*n); for j in 0..n { c[ci+j]+=aik*b[bk+j]; } } }
 let t:f64=c.iter().sum(); println!("{}",t); }
