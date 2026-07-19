# Optional Meson extension module for Vire: `vire.executable()` / `vire.static_library()`.
#
# This is the *ergonomic* layer over the same stable CLI the tested custom_target
# pattern uses (`vire build --emit=obj|staticlib --deps`). The custom_target pattern in
# example/meson.build needs NO installation and is the recommended integration; install
# this module only if you want the shorter `vire.executable(...)` spelling.
#
# Install: copy into your Meson's `mesonbuild/modules/` directory (find it with
#   python3 -c "import mesonbuild.modules, os; print(os.path.dirname(mesonbuild.modules.__file__))"
# ), then in meson.build:  vire = import('vire')
#
# API (mirrors Meson's own executable/static_library where it matters):
#   vire.executable(name, sources, c_sources: [], link_args: [], deps: [], pkg: [])
#   vire.static_library(name, sources, ...)
# `sources` are `.vr` files (each lowered to one C-ABI object); `c_sources` are plain
# C/C++ sources linked alongside; `pkg` names are resolved via `vire --pkg` (pkg-config).
#
# NOTE: Meson's module ABI shifts between releases; this targets the ExtensionModule API
# of Meson >= 0.64. If a method signature mismatches your Meson, fall back to the
# custom_target pattern (example/meson.build), which is version-robust and tested.

from __future__ import annotations

from mesonbuild.modules import ExtensionModule, ModuleInfo
from mesonbuild.interpreter.type_checking import NoneType
from mesonbuild import mesonlib


class VireModule(ExtensionModule):
    INFO = ModuleInfo('vire', '0.1.0')

    def __init__(self, interpreter):
        super().__init__(interpreter)
        self.methods.update({
            'executable': self.executable,
            'static_library': self.static_library,
        })

    # --- helpers -----------------------------------------------------------------

    def _find_vire(self, state):
        return state.find_program('vire')

    def _emit_objects(self, state, sources, emit, extra_args):
        """Lower each .vr source to one relocatable object via `vire build --emit=…`."""
        vire = self._find_vire(state)
        objs = []
        for src in mesonlib.extract_as_list(sources, sources):
            base = src if isinstance(src, str) else str(src)
            outname = mesonlib.os.path.basename(base) + ('.a' if emit == 'staticlib' else '.o')
            ct = state.backend.build.CustomTarget(
                outname,
                state.subdir,
                state.subproject,
                state.environment,
                [vire, 'build', '@INPUT@', '--emit=' + emit, '-o', '@OUTPUT@',
                 '--deps', '@DEPFILE@'] + list(extra_args),
                [base],
                [outname],
                depfile=outname + '.d',
            )
            objs.append(ct)
        return objs

    def _pkg_args(self, pkgs):
        args = []
        for p in pkgs:
            args += ['--pkg', p]
        return args

    # --- public methods ----------------------------------------------------------

    def executable(self, state, args, kwargs):
        name = args[0]
        sources = args[1] if len(args) > 1 else kwargs.get('sources', [])
        pkg = kwargs.get('pkg', [])
        c_sources = kwargs.get('c_sources', [])
        link_args = list(kwargs.get('link_args', [])) + ['-lm']
        objs = self._emit_objects(state, sources, 'obj', self._pkg_args(pkg))
        return self.interpreter.func_executable(
            None, [name] + list(c_sources),
            {'objects': objs, 'link_args': link_args, 'dependencies': kwargs.get('deps', [])},
        )

    def static_library(self, state, args, kwargs):
        name = args[0]
        sources = args[1] if len(args) > 1 else kwargs.get('sources', [])
        pkg = kwargs.get('pkg', [])
        # A .vr static library already merges into one .a; return it as a custom target.
        libs = self._emit_objects(state, sources, 'staticlib', self._pkg_args(pkg))
        return libs[0] if len(libs) == 1 else libs


def initialize(interp):
    return VireModule(interp)
