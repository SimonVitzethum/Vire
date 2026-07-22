#!/bin/sh
# Vire @vulkan suite (V2 + VS step 1: Vire is the shader language).
#
# Renders the triangle headless via a real Vulkan graphics pipeline
# (crates/driver/src/vk_runtime.c). vk_triangle() returns the centroid pixel packed
# as 0xRRGGBB, so a Vire program can check the color — including one produced by a
# Vire `@fragment fn` (compiled to SPIR-V via spirv-as; crates/backend/src/spirv.rs).
# Skips cleanly without a Vulkan runtime/device or spirv-as. See language/GPU-VULKAN.md.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

if ! ls /usr/lib/libvulkan.so* >/dev/null 2>&1 && ! ls /usr/lib/*/libvulkan.so* >/dev/null 2>&1; then
    echo "skip vire_vulkan (no libvulkan)"; exit 0
fi
command -v spirv-as >/dev/null 2>&1 || { echo "skip vire_vulkan (no spirv-as)"; exit 0; }
if command -v vulkaninfo >/dev/null 2>&1; then
    vulkaninfo --summary 2>/dev/null | grep -q deviceName || { echo "skip vire_vulkan (no Vulkan device)"; exit 0; }
fi

work="$(mktemp -d)"; pass=0; fail=0
case_() {
    name="$1"; f="$work/$name.vr"; cat > "$f"
    if ! "$vire" build "$f" -o "$work/$name" >/dev/null 2>"$work/e"; then
        if grep -qi "vulkan\|spirv" "$work/e"; then echo "skip $name (env: $(head -1 "$work/e"))"; return; fi
        echo "FAIL $name (build): $(head -1 "$work/e")"; fail=$((fail+1)); return
    fi
    out="$("$work/$name" 2>/dev/null | grep -v '^\[' | head -1)"
    if [ "$out" = "1" ]; then echo "ok   $name"; pass=$((pass+1))
    else echo "FAIL $name (got '$out', want '1')"; fail=$((fail+1)); fi
}

# Default fragment color (no @fragment shader) → orange centroid (~255,102,25).
case_ default_color <<'EOF'
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r > 200 { if g > 60 { if g < 140 { if b < 80 { ok = 1 } } } }
    print(ok)
}
EOF

# A Vire @fragment shader sets the color → the triangle renders in THAT color.
# green vec4(0.2,0.8,0.3,1) → centroid ~ (51,204,76). Proves Vire drives the shader.
case_ vire_fragment_shader <<'EOF'
@fragment
fn fs() -> Vec4 { vec4(0.2, 0.8, 0.3, 1.0) }
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r < 90 { if g > 150 { if b > 30 { if b < 120 { ok = 1 } } } }
    print(ok)
}
EOF

# A Vire @fragment with real arithmetic (not a constant): a binding + vector*scalar
# → OpCompositeConstruct/OpVectorTimesScalar in the emitted SPIR-V. (0.1,0.4,0.15,0.5)*2
# = (0.2,0.8,0.3,1.0) green. Proves shader *bodies* compile, not just constants.
case_ vire_fragment_compute <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut base = vec4(0.1, 0.4, 0.15, 0.5)
    base * 2.0
}
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r < 90 { if g > 150 { if b > 30 { if b < 120 { ok = 1 } } } }
    print(ok)
}
EOF

# Per-pixel computation: r = gl_FragCoord.x / 256 (a horizontal gradient). At the
# sampled centroid (x=128) r≈128 — a value derived from the pixel POSITION, not any
# constant in the shader (which only has 256.0, 0.8, 0.3). Proves fragment inputs.
case_ vire_fragment_fragcoord <<'EOF'
@fragment
fn fs() -> Vec4 { vec4(frag_x() / 256.0, 0.8, 0.3, 1.0) }
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r > 118 { if r < 138 { if g > 190 { if b > 60 { if b < 90 { ok = 1 } } } } }
    print(ok)
}
EOF

# A Vire @vertex shader TRANSFORMS the geometry: shifting every corner x+3 moves the
# triangle off-screen, so the centroid becomes the dark clear color. Proves the
# vertex stage is Vire-authored (both stages: @vertex + @fragment here).
case_ vire_vertex_shader <<'EOF'
@vertex
fn vs(pos: Vec2) -> Vec4 { vec4(pos.x + 3.0, pos.y, 0.0, 1.0) }
@fragment
fn fs() -> Vec4 { vec4(0.9, 0.2, 0.2, 1.0) }
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r < 40 { if g < 40 { if b < 40 { ok = 1 } } }   // off-screen → dark clear
    print(ok)
}
EOF

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
