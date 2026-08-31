/* spam.c - minimal C extension used to verify pip binary-wheel installs.
 * Compiled to spam.cp314-win_amd64.pyd (SOABI suffix) and packaged as
 * spam-1.0-cp314-cp314-win_amd64.whl; links the python314 shim like every
 * other extension built in bench\labs. */
#include <Python.h>

static PyObject *
spam_hello(PyObject *self, PyObject *Py_UNUSED(ignored))
{
    return PyUnicode_FromString("hello from spam!");
}

static PyObject *
spam_add(PyObject *self, PyObject *args)
{
    long a, b;
    if (!PyArg_ParseTuple(args, "ll", &a, &b)) {
        return NULL;
    }
    return PyLong_FromLong(a + b);
}

static PyMethodDef spam_methods[] = {
    {"hello", spam_hello, METH_NOARGS, "say hello"},
    {"add", spam_add, METH_VARARGS, "add two ints"},
    {NULL, NULL, 0, NULL},
};

static struct PyModuleDef spam_module = {
    PyModuleDef_HEAD_INIT,
    "spam",
    "spam test module",
    -1,
    spam_methods,
    NULL,
    NULL,
    NULL,
    NULL,
};

PyMODINIT_FUNC
PyInit_spam(void)
{
    return PyModule_Create(&spam_module);
}
