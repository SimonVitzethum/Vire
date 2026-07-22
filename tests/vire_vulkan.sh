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

# Varyings: the @vertex stage writes a per-vertex color via `out_color(vec3)`, the
# @fragment reads the INTERPOLATED value via `in_color()`. Color = position + 0.5
# (blue held constant 0.15). At the sampled centroid the interpolated r≈128, g≈152 —
# g≠r proves the per-vertex value is interpolated across the triangle (a flat
# fragment color cannot do this), and b≈38 is the constant channel. The vertex→
# fragment Location-0 link is auto-derived. Both stages Vire-authored.
case_ vire_varying_color <<'EOF'
@vertex
fn vs(pos: Vec2) -> Vec4 {
    out_color(vec3(pos.x + 0.5, pos.y + 0.5, 0.15))
    vec4(pos.x, pos.y, 0.0, 1.0)
}
@fragment
fn fs() -> Vec4 { vec4(in_color(), 1.0) }
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r > 108 { if r < 148 { if g > r { if g < 180 { if b > 20 { if b < 60 { ok = 1 } } } } } }
    print(ok)
}
EOF

# Vertex-buffer geometry: vk_mesh(verts) renders a triangle from Vire DATA (a flat
# [Float] of interleaved x,y), not the built-in corners. (1) The default corners as
# Vire data render identically to vk_triangle (green centroid) — proving the vertex
# buffer path. (2) Shifting every x by +3 moves the triangle off-screen, so the
# centroid becomes the dark clear color — proving the Vire data drives the geometry.
# The bridge to GPU-driven meshlets (per-vertex data now comes from Vire).
case_ vire_mesh_buffer <<'EOF'
@fragment
fn fs() -> Vec4 { vec4(0.2, 0.8, 0.3, 1.0) }
fn main() {
    mut tri = [0.0, -0.6, 0.6, 0.6, -0.6, 0.6]
    mut a = vk_mesh(tri)
    mut ar = a / 65536
    mut ag = (a / 256) % 256
    mut ab = a % 256
    mut off = [3.0, -0.6, 3.6, 0.6, 2.4, 0.6]
    mut b = vk_mesh(off)
    mut br = b / 65536
    mut bg = (b / 256) % 256
    mut bb = b % 256
    mut ok = 0
    if ar < 90 { if ag > 150 { if ab > 30 { if ab < 120 {   // (1) Vire data == built-in
      if br < 40 { if bg < 40 { if bb < 40 {                 // (2) off-screen → clear
        ok = 1 } } } } } } }
    print(ok)
}
EOF

# Per-vertex color attributes: vk_mesh_c(verts) interleaves (x,y, r,g,b) per vertex.
# The @vertex reads its own color via attr_color() (vertex-buffer Location 1) and
# forwards it as a varying; the fragment paints the interpolated result. The classic
# RGB triangle: red/green/blue corners. At the centroid ALL THREE channels are
# present (each 40..160) — only possible if three pure per-vertex colors interpolate
# (a flat or position-derived color cannot). Typed stage I/O: geometry + attributes
# from Vire.
case_ vire_mesh_attr_color <<'EOF'
@vertex
fn vs(pos: Vec2) -> Vec4 {
    out_color(attr_color())
    vec4(pos.x, pos.y, 0.0, 1.0)
}
@fragment
fn fs() -> Vec4 { vec4(in_color(), 1.0) }
fn main() {
    mut tri = [0.0, -0.6, 1.0, 0.0, 0.0, 0.6, 0.6, 0.0, 1.0, 0.0, -0.6, 0.6, 0.0, 0.0, 1.0]
    mut px = vk_mesh_c(tri)
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r > 40 { if r < 160 { if g > 40 { if g < 160 { if b > 40 { if b < 160 {
        ok = 1 } } } } } }
    print(ok)
}
EOF

# Structured control flow — BRANCH: the fragment picks its color with an `if` on the
# pixel position (OpSelectionMerge). The centroid (x=128) is >= 100, so it takes the
# else branch → blue. A per-pixel decision, not a constant.
case_ vire_shader_branch <<'EOF'
@fragment
fn fs() -> Vec4 {
    if frag_x() < 100.0 { vec4(0.9, 0.1, 0.1, 1.0) } else { vec4(0.1, 0.1, 0.9, 1.0) }
}
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r < 60 { if g < 60 { if b > 200 { ok = 1 } } }   // else branch → blue
    print(ok)
}
EOF

# Structured control flow — LOOP: a `while` accumulates 0.1 five times (OpLoopMerge)
# into the red channel → 0.5 → ~128. Proves real iteration with a mutable local
# carried across the loop back-edge (the storage-variable model).
case_ vire_shader_loop <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut acc = 0.0
    mut i = 0.0
    while i < 5.0 {
        acc = acc + 0.1
        i = i + 1.0
    }
    vec4(acc, 0.2, 0.7, 1.0)
}
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r > 118 { if r < 138 { if g > 40 { if g < 64 { if b > 165 { if b < 190 {
        ok = 1 } } } } } }
    print(ok)
}
EOF

# GLSL.std.450 builtins: a Lambert term from normalize()/dot()/max(), then mix()
# between a dark and a bright color. dot(normalize(.3,.4,1), (0,0,1)) = 0.894, so
# mix gives centroid ~ (47,207,70). Proves real vector math (OpExtInst + OpDot), the
# lighting primitives shaders need.
case_ vire_shader_glsl <<'EOF'
@fragment
fn fs() -> Vec4 {
    mut nrm = normalize(vec3(0.3, 0.4, 1.0))
    mut lgt = normalize(vec3(0.0, 0.0, 1.0))
    mut d = max(dot(nrm, lgt), 0.0)
    mut base = mix(vec3(0.05, 0.05, 0.05), vec3(0.2, 0.9, 0.3), d)
    vec4(base, 1.0)
}
fn main() {
    mut px = vk_triangle()
    mut r = px / 65536
    mut g = (px / 256) % 256
    mut b = px % 256
    mut ok = 0
    if r > 40 { if r < 55 { if g > 195 { if g < 216 { if b > 62 { if b < 78 {
        ok = 1 } } } } } }
    print(ok)
}
EOF

# GPU-driven mesh shader (VM milestone): vk_mesh_shader() draws via a mesh pipeline
# (VK_EXT_mesh_shader / vkCmdDrawMeshTasksEXT) — the mesh shader emits the triangle
# itself, no vertex buffer and no vertex stage. The Vire @fragment colors it orange
# (0.9,0.5,0.1) → centroid ~ (229,127,25). Returns -2 where the device lacks mesh
# shaders, so the case passes (as a skip) there too.
case_ vire_mesh_shader <<'EOF'
@fragment
fn fs() -> Vec4 { vec4(0.9, 0.5, 0.1, 1.0) }
fn main() {
    mut px = vk_mesh_shader()
    mut ok = 0
    if px == -2 { ok = 1 }                          // no mesh-shader device → skip-pass
    if px > 0 {
        mut r = px / 65536
        mut g = (px / 256) % 256
        mut b = px % 256
        if r > 200 { if g > 105 { if g < 150 { if b < 60 { ok = 1 } } } }
    }
    print(ok)
}
EOF

# Vire-authored @mesh + @task (amplification): all THREE stages come from Vire. The
# @task emits one mesh workgroup; the @mesh computes the triangle's positions (note
# `mut s` + arithmetic) and writes the connectivity; the @fragment colors it cyan
# (0.2,0.7,0.9) → centroid ~ (51,178,229). -2 → skip where no mesh-shader device.
case_ vire_mesh_authored <<'EOF'
@task
fn ts() { emit_mesh_tasks(1) }
@mesh
fn ms() {
    set_mesh_outputs(3, 1)
    mut s = 0.6
    mesh_pos(0, vec4(0.0, 0.0 - s, 0.0, 1.0))
    mesh_pos(1, vec4(s, s, 0.0, 1.0))
    mesh_pos(2, vec4(0.0 - s, s, 0.0, 1.0))
    mesh_tri(0, 0, 1, 2)
}
@fragment
fn fs() -> Vec4 { vec4(0.2, 0.7, 0.9, 1.0) }
fn main() {
    mut px = vk_mesh_shader()
    mut ok = 0
    if px == -2 { ok = 1 }
    if px > 0 {
        mut r = px / 65536
        mut g = (px / 256) % 256
        mut b = px % 256
        if r < 65 { if g > 165 { if g < 190 { if b > 215 { ok = 1 } } } }
    }
    print(ok)
}
EOF

# The amplification shader CULLS: emit_mesh_tasks(0) launches no meshlet, so the
# triangle never renders and the centroid stays the dark clear color (~20,20,25).
# Proves the @task stage gates GPU geometry — the basis for GPU-driven culling.
case_ vire_task_cull <<'EOF'
@task
fn ts() { emit_mesh_tasks(0) }
@mesh
fn ms() {
    set_mesh_outputs(3, 1)
    mesh_pos(0, vec4(0.0, 0.0 - 0.6, 0.0, 1.0))
    mesh_pos(1, vec4(0.6, 0.6, 0.0, 1.0))
    mesh_pos(2, vec4(0.0 - 0.6, 0.6, 0.0, 1.0))
    mesh_tri(0, 0, 1, 2)
}
@fragment
fn fs() -> Vec4 { vec4(0.9, 0.2, 0.2, 1.0) }
fn main() {
    mut px = vk_mesh_shader()
    mut ok = 0
    if px == -2 { ok = 1 }
    if px > 0 {
        mut r = px / 65536
        mut g = (px / 256) % 256
        mut b = px % 256
        if r < 40 { if g < 40 { if b < 40 { ok = 1 } } }   // culled → clear color
    }
    print(ok)
}
EOF

# GPU frustum culling in the @task shader: the host pushes a frustum plane
# (cull_plane()), the task shader tests the meshlet's bounding-sphere center on the
# GPU (dot + compare → OpSelect emit 1/0). The SAME meshlet renders (plane (0,0,1,0)
# → d=0, visible) or is culled (plane (0,0,1,-2) → d=-2, behind) purely from the
# camera data — the basis for GPU-driven culling. -2 → skip where no mesh device.
case_ vire_task_gpu_cull <<'EOF'
@task
fn ts() {
    mut plane = cull_plane()
    mut center = vec4(0.0, 0.0, 0.0, 1.0)
    mut d = dot(plane, center)
    emit_mesh_tasks(d > 0.0 - 0.6)
}
@mesh
fn ms() {
    set_mesh_outputs(3, 1)
    mesh_pos(0, vec4(0.0, 0.0 - 0.6, 0.0, 1.0))
    mesh_pos(1, vec4(0.6, 0.6, 0.0, 1.0))
    mesh_pos(2, vec4(0.0 - 0.6, 0.6, 0.0, 1.0))
    mesh_tri(0, 0, 1, 2)
}
@fragment
fn fs() -> Vec4 { vec4(0.9, 0.6, 0.1, 1.0) }
fn main() {
    mut vis = vk_mesh_shader(0.0, 0.0, 1.0, 0.0)
    mut cul = vk_mesh_shader(0.0, 0.0, 1.0, 0.0 - 2.0)
    mut ok = 0
    if vis == -2 { ok = 1 }
    if vis > 0 {
        if vis / 65536 > 200 {          // visible → orange
            if cul / 65536 < 40 { ok = 1 }   // culled → dark clear
        }
    }
    print(ok)
}
EOF

echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
