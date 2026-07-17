#include <cstdio>
#include <cstdint>
struct Node { Node *l, *r; };
int check(Node* n) { return n->l == nullptr ? 1 : 1 + check(n->l) + check(n->r); }
Node* make(int d) {
    Node* n = new Node{nullptr, nullptr};
    if (d > 0) { n->l = make(d-1); n->r = make(d-1); }
    return n;
}
void freeTree(Node* n) { if (n->l) { freeTree(n->l); freeTree(n->r); } delete n; }
int main() {
    int maxDepth = 18; int64_t sum = 0;
    for (int depth = 4; depth <= maxDepth; depth += 2) {
        int iterations = 1 << (maxDepth - depth + 4);
        int64_t chk = 0;
        for (int i = 0; i < iterations; i++) { Node* t = make(depth); chk += check(t); freeTree(t); }
        sum += chk;
    }
    printf("%lld\n", (long long) sum);
}
