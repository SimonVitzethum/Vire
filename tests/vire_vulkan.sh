#!/bin/sh
# Vire @vulkan suite (V2 step 1: headless, self-verifying triangle).
#
# Drives a real Vulkan graphics pipeline from a Vire program: instance/device/
# render-pass/pipeline/draw + readback (crates/driver/src/vk_runtime.c). The
# runtime self-verifies the rendered pixels and returns 1; the kernel here just
# prints it. Skips cleanly if there is no Vulkan runtime/device (like vire_gpu.sh
# skips without a GPU). See language/GPU-VULKAN.md.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
vire="$root/target/release/vire"
[ -x "$vire" ] || { echo "vire missing — run 'cargo build --release -p vire'"; exit 1; }

# Skip if no Vulkan loader or no device.
if ! ls /usr/lib/libvulkan.so* >/dev/null 2>&1 && ! ls /usr/lib/*/libvulkan.so* >/dev/null 2>&1; then
    echo "skip vire_vulkan (no libvulkan)"; exit 0
fi
if command -v vulkaninfo >/dev/null 2>&1; then
    vulkaninfo --summary 2>/dev/null | grep -q deviceName || { echo "skip vire_vulkan (no Vulkan device)"; exit 0; }
fi

work="$(mktemp -d)"; pass=0; fail=0
cat > "$work/tri.vr" <<'EOF'
fn main() { print(vk_triangle()) }
EOF
if ! "$vire" build "$work/tri.vr" -o "$work/tri" >/dev/null 2>"$work/e"; then
    # A missing libvulkan at link time = environment skip, not a failure.
    if grep -qi "vulkan" "$work/e"; then echo "skip vire_vulkan (link: $(head -1 "$work/e"))"; rm -rf "$work"; exit 0; fi
    echo "FAIL triangle (build): $(head -1 "$work/e")"; rm -rf "$work"; exit 1
fi
out="$("$work/tri" 2>/dev/null | grep -v '^\[' | head -1)"
if [ "$out" = "1" ]; then
    echo "ok   triangle (rendered + pixel-verified)"; pass=1
else
    echo "FAIL triangle (got '$out', want '1')"; fail=1
fi
echo "---"
echo "$pass passed, $fail failed"
rm -rf "$work"
[ "$fail" -eq 0 ]
