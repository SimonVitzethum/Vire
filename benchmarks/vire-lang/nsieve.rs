fn main() {
    let n = 20000000usize;
    let mut flags = vec![0i64; n];
    for i in 2..n { flags[i] = 1; }
    let mut count = 0i64;
    let mut p = 2;
    while p < n {
        if flags[p] == 1 { count += 1; let mut k = p+p; while k < n { flags[k]=0; k+=p; } }
        p += 1;
    }
    println!("{}", count);
}
