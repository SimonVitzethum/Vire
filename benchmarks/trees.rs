struct Node { l: Option<Box<Node>>, r: Option<Box<Node>> }
fn check(n: &Node) -> i64 {
    match &n.l { None => 1, Some(l) => 1 + check(l) + check(n.r.as_ref().unwrap()) }
}
fn make(d: i32) -> Box<Node> {
    if d == 0 { Box::new(Node { l: None, r: None }) }
    else { Box::new(Node { l: Some(make(d-1)), r: Some(make(d-1)) }) }
}
fn main() {
    let max_depth = 18i32; let mut sum: i64 = 0;
    let mut depth = 4;
    while depth <= max_depth {
        let iterations = 1i64 << (max_depth - depth + 4);
        let mut chk: i64 = 0;
        for _ in 0..iterations { chk += check(&make(depth)); }
        sum += chk;
        depth += 2;
    }
    println!("{}", sum);
}
