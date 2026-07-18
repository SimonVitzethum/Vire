/* Eingebaute Python-Brücke: einmal hier, damit Vire-Nutzer Python-Bibliotheken
 * OHNE eigenen C-Code aufrufen können. Wird automatisch mitkompiliert+gelinkt,
 * sobald ein Programm `vire_py_*` deklariert. Nimmt einen Vire-Ausdruck als Text
 * (mit gebundener Variable `x`), wertet ihn im Python-Interpreter aus und gibt
 * das Ergebnis als Skalar zurück — reicht, um jede Python-Lib zu erreichen
 * (`__import__('numpy').linalg.norm(...)` etc.). */
#include <Python.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

/* Vire-String-Layout (muss zum Runtime-JStr passen: Header + len + bytes). */
typedef struct {
    int64_t refcount;
    int64_t rcflags;
    void *vtable;
    int64_t len;
    unsigned char bytes[];
} VStr;

static int g_py_ready = 0;
static void vire_py_ensure(void) {
    if (!g_py_ready) {
        Py_Initialize();
        g_py_ready = 1;
    }
}

static char *vstr_dup(const VStr *s) {
    char *c = (char *)malloc((size_t)s->len + 1);
    memcpy(c, s->bytes, (size_t)s->len);
    c[s->len] = 0;
    return c;
}

/* Wertet den Python-Ausdruck `code` aus (mit `x` als float gebunden) → double. */
double vire_py_eval_f(const VStr *code, double x) {
    vire_py_ensure();
    char *c = vstr_dup(code);
    PyObject *g = PyDict_New();
    PyDict_SetItemString(g, "__builtins__", PyEval_GetBuiltins());
    PyObject *xo = PyFloat_FromDouble(x);
    PyDict_SetItemString(g, "x", xo);
    PyObject *r = PyRun_String(c, Py_eval_input, g, g);
    double d = 0.0;
    if (r) {
        d = PyFloat_AsDouble(r);
    } else {
        PyErr_Print();
    }
    Py_XDECREF(r);
    Py_DECREF(xo);
    Py_DECREF(g);
    free(c);
    return d;
}

/* Wie oben, aber `x` als int gebunden → int64. */
int64_t vire_py_eval_i(const VStr *code, int64_t x) {
    vire_py_ensure();
    char *c = vstr_dup(code);
    PyObject *g = PyDict_New();
    PyDict_SetItemString(g, "__builtins__", PyEval_GetBuiltins());
    PyObject *xo = PyLong_FromLongLong((long long)x);
    PyDict_SetItemString(g, "x", xo);
    PyObject *r = PyRun_String(c, Py_eval_input, g, g);
    int64_t v = 0;
    if (r) {
        v = (int64_t)PyLong_AsLongLong(r);
    } else {
        PyErr_Print();
    }
    Py_XDECREF(r);
    Py_DECREF(xo);
    Py_DECREF(g);
    free(c);
    return v;
}
