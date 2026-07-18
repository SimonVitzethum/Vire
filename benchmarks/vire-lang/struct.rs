struct V { x: i64, y: i64, z: i64 }
fn main() {
    let mut s: i64 = 0;
    let mut i: i64 = 0;
    while i < 100000000 {
        let v = V { x: i, y: i*2, z: i*3 };
        s = (s + v.x + v.y + v.z) % 1000000007;
        i += 1;
    }
    println!("{}", s);
}
