# Vire + Meson interop, using only the stock Meson DSL (no plugin to install).
#
# A whole `.vr` program lowers to ONE relocatable object exposing C-ABI symbols
# (the runtime `main` is included), which Meson links like any other object — here
# together with a C object (util.c). Rebuilds are incremental via the `--deps`
# depfile: touch main.vr or a header it pulls in and Ninja recompiles.
project('vire-meson-example', 'c', meson_version: '>=1.1.0')

vire = find_program('vire', required: true)

# .vr  ->  main.vr.o   (whole-program object, C-ABI symbols, includes `main`)
main_obj = custom_target(
  'main.vr.o',
  input: 'main.vr',
  output: 'main.vr.o',
  command: [vire, 'build', '@INPUT@', '--emit=obj', '-o', '@OUTPUT@', '--deps', '@DEPFILE@'],
  depfile: 'main.vr.o.d',
)

# Link the Vire object with a plain C object into the final executable.
# `-lm` because the Vire runtime uses libm math intrinsics.
executable(
  'app',
  'util.c',
  objects: main_obj,
  link_args: ['-lm'],
)
