trait Op { fn apply(&self, x:i64)->i64; }
struct AddOp{k:i64} impl Op for AddOp { fn apply(&self,x:i64)->i64 { x+self.k } }
struct MulOp{k:i64} impl Op for MulOp { fn apply(&self,x:i64)->i64 { x*self.k } }
fn run(o:&dyn Op, iters:i64)->i64 { let mut acc=0i64; let mut i=0i64; while i<iters { acc=o.apply(acc)%1000000; i+=1; } acc }
fn main(){ let a=AddOp{k:3}; let m=MulOp{k:7}; println!("{}",run(&a,50000000)+run(&m,50000000)); }
