class Vec2 { int x, y; Vec2(int x,int y){this.x=x;this.y=y;} }
public class IpEsc {
    // groß genug, dass der Inliner es NICHT inlinet (>16 Statements)
    static int score(Vec2 a, Vec2 b) {
        int s = 0;
        s += a.x*b.x; s += a.y*b.y; s += a.x+b.x; s += a.y+b.y;
        s += a.x-b.x; s += a.y-b.y; s += a.x*a.y; s += b.x*b.y;
        s += a.x*2; s += b.y*3; s += a.y*5; s += b.x*7;
        return s;
    }
    public static void main(String[] args) {
        Vec2 p = new Vec2(3, 4);
        Vec2 q = new Vec2(2, 3);
        System.out.println(score(p, q));
    }
}
