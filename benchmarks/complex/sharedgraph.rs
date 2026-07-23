use std::rc::Rc;
use std::cell::RefCell;
type Link = Option<Rc<RefCell<Node>>>;
struct Node { id: i64, next: Link, side: Link }
fn chain(len: i64) -> Link {
    if len == 0 { None } else { Some(Rc::new(RefCell::new(Node{ id: len, next: chain(len-1), side: None }))) }
}
fn main() {
    let mut sum: i64 = 0;
    let mut t = 0;
    while t < 400000 {
        let h = chain(20).unwrap();
        // share + mutate: side = grandchild
        let mut c = h.clone();
        loop {
            let nx = c.borrow().next.clone();
            match nx {
                Some(n) => {
                    let gc = n.borrow().next.clone();
                    if gc.is_some() { c.borrow_mut().side = gc; }
                    c = n;
                }
                None => break,
            }
        }
        // cycle: last.next = h
        let mut last = h.clone();
        loop { let nx = last.borrow().next.clone(); match nx { Some(n)=>last=n, None=>break } }
        last.borrow_mut().next = Some(h.clone());
        let sid = h.borrow().side.as_ref().map(|s| s.borrow().id).unwrap_or(0);
        sum = (sum + h.borrow().id + sid) % 1000000007;
        // break the cycle so Rc can free (Rust has no cycle collector)
        last.borrow_mut().next = None;
        t += 1;
    }
    println!("{}", sum);
}
