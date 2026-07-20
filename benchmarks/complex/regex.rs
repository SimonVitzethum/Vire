fn matchhere(pat: &[i64], pi: i64, pl: i64, tx: &[i64], ti: i64, tl: i64) -> i64 {
    if pi >= pl { return 1; }
    if pi + 1 < pl && pat[(pi+1) as usize] == 42 { return matchstar(pat[pi as usize], pat, pi+2, pl, tx, ti, tl); }
    if ti < tl && (pat[pi as usize] == 46 || pat[pi as usize] == tx[ti as usize]) { return matchhere(pat, pi+1, pl, tx, ti+1, tl); }
    0
}
fn matchstar(c: i64, pat: &[i64], pi: i64, pl: i64, tx: &[i64], ti: i64, tl: i64) -> i64 {
    let mut t = ti;
    loop {
        if matchhere(pat, pi, pl, tx, t, tl) == 1 { return 1; }
        if t < tl && (c == 46 || c == tx[t as usize]) { t += 1; } else { return 0; }
    }
}
fn search(pat: &[i64], pl: i64, tx: &[i64], tl: i64) -> i64 {
    let mut ti = 0;
    while ti <= tl { if matchhere(pat, 0, pl, tx, ti, tl) == 1 { return 1; } ti += 1; }
    0
}
fn main() {
    let pat: [i64;16] = [97,46,42,98,46,42,99,46,42,100,46,42,97,46,42,98];
    let pl = 16i64; let tl = 40i64;
    let mut tx = vec![0i64; tl as usize];
    let mut seed = 20240101i64; let mut count = 0i64; let n = 2000000i64;
    for _ in 0..n {
        for j in 0..tl as usize { seed = (seed*1103515245+12345)%2147483648; tx[j] = 97 + seed%4; }
        count += search(&pat, pl, &tx, tl);
    }
    println!("{}", count);
}
