fn main(){ let mut s=0i64; let mut i=0i64; while i<30000000 { let mut x=i; let mut c=0i64; while x>0 { c+=x&1; x/=2; } s+=c; i+=1; } println!("{}",s); }
