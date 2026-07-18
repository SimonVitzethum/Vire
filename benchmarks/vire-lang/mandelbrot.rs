fn main() {
    let n = 2000i64;
    let mut count = 0i64;
    for py in 0..n {
        for px in 0..n {
            let cr = px as f64 * 3.0 / n as f64 - 2.0;
            let ci = py as f64 * 2.0 / n as f64 - 1.0;
            let (mut zr, mut zi) = (0.0f64, 0.0f64);
            let mut esc = 0i64;
            let mut i = 0;
            while i < 50 {
                let zr2 = zr*zr - zi*zi + cr;
                zi = 2.0*zr*zi + ci;
                zr = zr2;
                if zr*zr + zi*zi > 4.0 { esc = 1; i = 50; } else { i += 1; }
            }
            count += 1 - esc;
        }
    }
    println!("{}", count);
}
