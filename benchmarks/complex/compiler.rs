enum Node { Num(i64), Op(i64, Box<Node>, Box<Node>) }
fn gen(buf: &mut [i64], p: &mut i64, depth: i64, s: &mut i64) {
    *s = (*s * 1103515245 + 12345) % 2147483648;
    if depth <= 0 || *s % 3 == 0 {
        let num = *s % 90 + 10;
        buf[*p as usize] = num / 10 + 48; *p += 1;
        buf[*p as usize] = num % 10 + 48; *p += 1;
    } else {
        buf[*p as usize] = 40; *p += 1;
        gen(buf, p, depth - 1, s);
        *s = (*s * 1103515245 + 12345) % 2147483648;
        let mut op = 43; if *s % 3 == 1 { op = 45; } if *s % 3 == 2 { op = 42; }
        buf[*p as usize] = op; *p += 1;
        gen(buf, p, depth - 1, s);
        buf[*p as usize] = 41; *p += 1;
    }
}
fn parse(buf: &[i64], p: &mut i64) -> Box<Node> {
    let c = buf[*p as usize];
    if c == 40 {
        *p += 1;
        let l = parse(buf, p);
        let op = buf[*p as usize]; *p += 1;
        let r = parse(buf, p);
        *p += 1;
        Box::new(Node::Op(op, l, r))
    } else {
        let mut v = 0i64;
        while buf[*p as usize] >= 48 && buf[*p as usize] <= 57 { v = v * 10 + (buf[*p as usize] - 48); *p += 1; }
        Box::new(Node::Num(v))
    }
}
fn eval(n: &Node) -> i64 {
    match n {
        Node::Num(v) => *v,
        Node::Op(k, l, r) => {
            let a = eval(l); let b = eval(r);
            if *k == 43 { (a + b) % 1000000007 }
            else if *k == 45 { let mut x = (a - b) % 1000000007; if x < 0 { x += 1000000007; } x }
            else { a * b % 1000000007 }
        }
    }
}
fn main() {
    let mut buf = vec![0i64; 2000000];
    let mut checksum = 0i64;
    for it in 0..400i64 {
        let mut s = it * 2654435761 + 12345;
        let mut p = 0i64;
        gen(&mut buf, &mut p, 15, &mut s);
        let mut p2 = 0i64;
        let ast = parse(&buf, &mut p2);
        checksum = (checksum + eval(&ast)) % 1000000007;
    }
    println!("{}", checksum);
}
