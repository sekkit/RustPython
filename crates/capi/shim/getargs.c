/*
 * Variadic C-API entry points (CPython Python/getargs.c + Objects/abstract.c
 * equivalents).
 *
 * Stable Rust cannot *define* C-variadic functions (`c_variadic` is
 * unstable), so these thin wrappers snapshot the variadic arguments into a
 * uniform array of `uintptr_t` slots and hand them to the Rust
 * implementation in crates/capi/src/arg.rs.
 *
 * All variadic arguments occupy exactly one 8-byte slot on the ABIs we
 * target (Win64, SysV x86-64), so extracting every slot as `uintptr_t` is
 * layout-correct. The number of slots each format string consumes is
 * computed by rp_count_slots(), which must stay in sync with the format
 * walker in arg.rs.
 */
#include <stdarg.h>
#include <stdint.h>

/* Rust implementations (see crates/capi/src/arg.rs). */
int rp_va_parse_tuple(void *args, const char *format, const uintptr_t *slots, int nslots);
int rp_va_parse_tuple_and_keywords(void *args, void *kwdict, const char *format,
                                   const char *const *kwlist, const uintptr_t *slots,
                                   int nslots);
int rp_va_unpack_tuple(void *args, const char *name, intptr_t min, intptr_t max,
                       const uintptr_t *slots, int nslots);
void *rp_va_build_value(const char *format, const uintptr_t *slots, int nslots);
void *rp_va_call_function(void *callable, const char *format, const uintptr_t *slots,
                          int nslots);
void *rp_va_call_method(void *obj, const char *name, const char *format,
                        const uintptr_t *slots, int nslots);
void *rp_va_call_function_objargs(void *callable, const uintptr_t *slots, int nslots);
void *rp_va_call_method_objargs(void *obj, const char *name, const uintptr_t *slots,
                                int nslots);
void *rp_va_err_format(void *exception, const char *format, const uintptr_t *slots,
                       int nslots);

#define RP_MAX_SLOTS 64

/* Number of variadic slots consumed by a printf-style format string (for
 * PyErr_Format). Keep in sync with the formatter in arg.rs. */
static int rp_count_printf_slots(const char *format) {
    int n = 0;
    const char *p;
    for (p = format; *p; p++) {
        if (*p != '%') {
            continue;
        }
        p++;
        if (*p == '%') {
            continue;
        }
        while (*p == '-' || *p == '+' || *p == ' ' || *p == '#' || *p == '0') {
            p++;
        }
        if (*p == '*') {
            n++;
            p++;
        } else {
            while (*p >= '0' && *p <= '9') {
                p++;
            }
        }
        if (*p == '.') {
            p++;
            if (*p == '*') {
                n++;
                p++;
            } else {
                while (*p >= '0' && *p <= '9') {
                    p++;
                }
            }
        }
        while (*p == 'l' || *p == 'h' || *p == 'z' || *p == 't' || *p == 'j' ||
               *p == 'L' || *p == 'q') {
            p++;
        }
        if (*p == 'V') {
            n += 2;
        } else if (*p != '\0') {
            n += 1;
        }
        if (*p == '\0') {
            break;
        }
    }
    return n;
}

/* Number of variadic slots consumed by a format string. Keep in sync with
 * the format walker in arg.rs. */
static int rp_count_slots(const char *format) {
    int n = 0;
    const char *p;
    for (p = format; *p; p++) {
        char c = *p;
        if (c == ' ' || c == '(' || c == ')' || c == '[' || c == ']' ||
            c == '{' || c == '}' || c == ',' ||
            c == '|' || c == '$' || c == ':' || c == ';') {
            continue;
        }
        if (c == 'O') {
            if (p[1] == '!' || p[1] == '&') {
                p++;
                n += 2;
            } else {
                n += 1;
            }
            continue;
        }
        if (c == 'e' && (p[1] == 's' || p[1] == 't')) {
            p++;
            if (p[1] == '#') {
                p++;
                n += 3;
            } else {
                n += 2;
            }
            continue;
        }
        if ((c == 's' || c == 'z' || c == 'y' || c == 'u' || c == 'w' || c == 't') &&
            p[1] == '#') {
            p++;
            n += 2;
            continue;
        }
        n += 1; /* every other conversion code takes exactly one slot */
    }
    return n;
}

int PyArg_ParseTuple(void *args, const char *format, ...) {
    va_list ap;
    uintptr_t slots[RP_MAX_SLOTS];
    int n;
    va_start(ap, format);
    n = rp_count_slots(format == NULL ? "" : format);
    if (n > RP_MAX_SLOTS) {
        n = RP_MAX_SLOTS;
    }
    for (int i = 0; i < n; i++) {
        slots[i] = va_arg(ap, uintptr_t);
    }
    va_end(ap);
    return rp_va_parse_tuple(args, format, slots, n);
}

int PyArg_ParseTupleAndKeywords(void *args, void *kwdict, const char *format,
                                const char *const *kwlist, ...) {
    va_list ap;
    uintptr_t slots[RP_MAX_SLOTS];
    int n;
    va_start(ap, kwlist);
    n = rp_count_slots(format == NULL ? "" : format);
    if (n > RP_MAX_SLOTS) {
        n = RP_MAX_SLOTS;
    }
    for (int i = 0; i < n; i++) {
        slots[i] = va_arg(ap, uintptr_t);
    }
    va_end(ap);
    return rp_va_parse_tuple_and_keywords(args, kwdict, format, kwlist, slots, n);
}

/* Provided by rustpython-capi (crates/capi/src/tupleobject.rs). */
intptr_t PyTuple_Size(void *obj);

int PyArg_UnpackTuple(void *args, const char *name, intptr_t min, intptr_t max, ...) {
    va_list ap;
    uintptr_t slots[RP_MAX_SLOTS];
    int n = 0;
    intptr_t nargs = PyTuple_Size(args);
    if (nargs < 0) {
        return 0; /* exception already set */
    }
    if (nargs > RP_MAX_SLOTS) {
        nargs = RP_MAX_SLOTS;
    }
    va_start(ap, max);
    for (intptr_t i = 0; i < nargs; i++) {
        slots[i] = va_arg(ap, uintptr_t);
        n++;
    }
    va_end(ap);
    return rp_va_unpack_tuple(args, name, min, max, slots, n);
}

void *Py_BuildValue(const char *format, ...) {
    va_list ap;
    uintptr_t slots[RP_MAX_SLOTS];
    int n;
    va_start(ap, format);
    n = rp_count_slots(format == NULL ? "" : format);
    if (n > RP_MAX_SLOTS) {
        n = RP_MAX_SLOTS;
    }
    for (int i = 0; i < n; i++) {
        slots[i] = va_arg(ap, uintptr_t);
    }
    va_end(ap);
    return rp_va_build_value(format, slots, n);
}

void *PyObject_CallFunction(void *callable, const char *format, ...) {
    va_list ap;
    uintptr_t slots[RP_MAX_SLOTS];
    int n;
    va_start(ap, format);
    n = rp_count_slots(format == NULL ? "" : format);
    if (n > RP_MAX_SLOTS) {
        n = RP_MAX_SLOTS;
    }
    for (int i = 0; i < n; i++) {
        slots[i] = va_arg(ap, uintptr_t);
    }
    va_end(ap);
    return rp_va_call_function(callable, format, slots, n);
}

void *PyObject_CallMethod(void *obj, const char *name, const char *format, ...) {
    va_list ap;
    uintptr_t slots[RP_MAX_SLOTS];
    int n;
    va_start(ap, format);
    n = rp_count_slots(format == NULL ? "" : format);
    if (n > RP_MAX_SLOTS) {
        n = RP_MAX_SLOTS;
    }
    for (int i = 0; i < n; i++) {
        slots[i] = va_arg(ap, uintptr_t);
    }
    va_end(ap);
    return rp_va_call_method(obj, name, format, slots, n);
}

void *PyObject_CallFunctionObjArgs(void *callable, ...) {
    va_list ap;
    uintptr_t slots[RP_MAX_SLOTS];
    int n = 0;
    va_start(ap, callable);
    while (n < RP_MAX_SLOTS) {
        uintptr_t v = va_arg(ap, uintptr_t);
        if (v == 0) {
            break;
        }
        slots[n++] = v;
    }
    va_end(ap);
    return rp_va_call_function_objargs(callable, slots, n);
}

void *PyObject_CallMethodObjArgs(void *obj, const char *name, ...) {
    va_list ap;
    uintptr_t slots[RP_MAX_SLOTS];
    int n = 0;
    va_start(ap, name);
    while (n < RP_MAX_SLOTS) {
        uintptr_t v = va_arg(ap, uintptr_t);
        if (v == 0) {
            break;
        }
        slots[n++] = v;
    }
    va_end(ap);
    return rp_va_call_method_objargs(obj, name, slots, n);
}

void *PyErr_Format(void *exception, const char *format, ...) {
    va_list ap;
    uintptr_t slots[RP_MAX_SLOTS];
    int n;
    va_start(ap, format);
    n = rp_count_printf_slots(format == NULL ? "" : format);
    if (n > RP_MAX_SLOTS) {
        n = RP_MAX_SLOTS;
    }
    for (int i = 0; i < n; i++) {
        slots[i] = va_arg(ap, uintptr_t);
    }
    va_end(ap);
    return rp_va_err_format(exception, format, slots, n);
}
