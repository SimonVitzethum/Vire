#include <Python.h>
/* Vire → Python: ruft eine Python-Bibliothek (math) über die CPython-C-API. */
extern double py_math_sqrt_times(double x, double k) {
    Py_Initialize();
    PyObject *math = PyImport_ImportModule("math");
    PyObject *fn = PyObject_GetAttrString(math, "sqrt");
    PyObject *arg = PyFloat_FromDouble(x);
    PyObject *res = PyObject_CallOneArg(fn, arg);
    double d = PyFloat_AsDouble(res);
    Py_DECREF(arg); Py_DECREF(res); Py_DECREF(fn); Py_DECREF(math);
    Py_Finalize();
    return d * k;
}
