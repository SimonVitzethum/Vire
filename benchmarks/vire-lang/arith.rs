fn main() {
    let mut s: i64 = 0;
    let mut i: i64 = 0;
    while i < 300000000 {
        s = (s + i * 3 + 7) % 1000000007;
        i += 1;
    }
    println!("{}", s);
}
