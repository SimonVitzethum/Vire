/* Host-side CUDA Driver-API runtime for Vire @gpu kernels.
 *
 * Compiled together with the generated `jrt_gpu_ptx` (the embedded PTX text)
 * and the per-kernel launch stubs (see backend/src/nvptx.rs). The stubs are
 * cuda.h-free and talk to the GPU only through the small `jrt_gpu_*` ABI below.
 *
 * v1 scope: one device (0), one lazily-created context + module, synchronous
 * launches (each launch syncs). Every array argument is uploaded before and
 * copied back after the launch (treated as in/out). This runs on the "GPU
 * track" — it is deliberately NOT part of the bit-identical CPU oracle, since
 * GPU floating point differs from CPU (different FMA/rounding/reduction order).
 *
 * Design adapted from NVlabs/cuda-oxide (Apache-2.0); see third_party/cuda-oxide.
 */
#include <cuda.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

/* The embedded PTX module text, defined by the generated stubs translation
 * unit (as `const char jrt_gpu_ptx[] = "...";`). */
extern const char jrt_gpu_ptx[];

static CUcontext g_ctx;
static CUmodule g_mod;
static int g_ready = 0;

static void gpu_check(CUresult r, const char *what) {
    if (r != CUDA_SUCCESS) {
        const char *s = "?";
        cuGetErrorString(r, &s);
        fprintf(stderr, "vire @gpu: %s failed: %s\n", what, s);
        exit(1);
    }
}

void jrt_gpu_ensure(void) {
    if (g_ready) return;
    gpu_check(cuInit(0), "cuInit");
    CUdevice dev;
    gpu_check(cuDeviceGet(&dev, 0), "cuDeviceGet");
    gpu_check(cuCtxCreate(&g_ctx, NULL, 0, dev), "cuCtxCreate");
    gpu_check(cuModuleLoadData(&g_mod, jrt_gpu_ptx), "cuModuleLoadData");
    g_ready = 1;
}

void *jrt_gpu_func(const char *name) {
    CUfunction fn;
    gpu_check(cuModuleGetFunction(&fn, g_mod, name), "cuModuleGetFunction");
    return (void *)fn;
}

void *jrt_gpu_upload(void *host, int64_t bytes) {
    CUdeviceptr d = 0;
    if (bytes > 0) {
        gpu_check(cuMemAlloc(&d, (size_t)bytes), "cuMemAlloc");
        gpu_check(cuMemcpyHtoD(d, host, (size_t)bytes), "cuMemcpyHtoD");
    }
    return (void *)(uintptr_t)d;
}

void jrt_gpu_download(void *host, void *dev, int64_t bytes) {
    if (bytes > 0 && dev) {
        gpu_check(cuMemcpyDtoH(host, (CUdeviceptr)(uintptr_t)dev, (size_t)bytes), "cuMemcpyDtoH");
    }
}

void jrt_gpu_free(void *dev) {
    if (dev) cuMemFree((CUdeviceptr)(uintptr_t)dev);
}

void jrt_gpu_launch(void *fn, int64_t n_threads, void **params, int nparams) {
    (void)nparams;
    if (n_threads <= 0) return;
    unsigned block = 256;
    unsigned grid = (unsigned)((n_threads + block - 1) / block);
    gpu_check(cuLaunchKernel((CUfunction)fn, grid, 1, 1, block, 1, 1, 0, 0, params, 0), "cuLaunchKernel");
    gpu_check(cuCtxSynchronize(), "cuCtxSynchronize");
}

/* Length (element count) of a Vire array object: the i64 at header offset 16. */
int64_t jrt_gpu_arrlen(void *arr) {
    return *(int64_t *)((char *)arr + 16);
}
