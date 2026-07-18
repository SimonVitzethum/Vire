struct Tree { l: Option<Box<Tree>>, r: Option<Box<Tree>> }
fn make(d: i64) -> Tree {
    if d == 0 { Tree { l: None, r: None } }
    else { Tree { l: Some(Box::new(make(d-1))), r: Some(Box::new(make(d-1))) } }
}
fn check(t: &Tree, d: i64) -> i64 {
    if d == 0 { 1 } else { 1 + check(t.l.as_ref().unwrap(), d-1) + check(t.r.as_ref().unwrap(), d-1) }
}
fn main() {
    let mut sum = 0i64;
    for _ in 0..60 { sum += check(&make(16), 16); }
    println!("{}", sum);
}
