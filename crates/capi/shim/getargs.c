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

/* These functions are defined in C, so rustc's #[no_mangle]-based export
 * machinery does not apply; export them explicitly so the cdylib's export
 * table contains them (needed for ctypes.pythonapi and .pyd loading). */
#ifdef _WIN32
#define RP_EXPORT __declspec(dllexport)
#else
#define RP_EXPORT __attribute__((visibility("default")))
#endif

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

RP_EXPORT int PyArg_ParseTuple(void *args, const char *format, ...) {
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

RP_EXPORT int PyArg_ParseTupleAndKeywords(void *args, void *kwdict, const char *format,
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

RP_EXPORT int PyArg_UnpackTuple(void *args, const char *name, intptr_t min, intptr_t max, ...) {
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

RP_EXPORT void *Py_BuildValue(const char *format, ...) {
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

RP_EXPORT void *PyObject_CallFunction(void *callable, const char *format, ...) {
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

RP_EXPORT void *PyObject_CallMethod(void *obj, const char *name, const char *format, ...) {
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

RP_EXPORT void *PyObject_CallFunctionObjArgs(void *callable, ...) {
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

RP_EXPORT void *PyObject_CallMethodObjArgs(void *obj, const char *name, ...) {
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

RP_EXPORT void *PyErr_Format(void *exception, const char *format, ...) {
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

/* PyUnicode_FromFormat (Objects/unicodeobject.c): build a unicode string
 * from a printf-style format. Uses the same slot-snapshotting pattern. */
void *rp_va_unicode_from_format(const char *format, const uintptr_t *slots, int nslots);

RP_EXPORT void *PyUnicode_FromFormat(const char *format, ...) {
    va_list ap;
    uintptr_t slots[RP_MAX_SLOTS];
    int n;
    if (format == NULL) {
        return NULL;
    }
    va_start(ap, format);
    n = rp_count_printf_slots(format);
    if (n > RP_MAX_SLOTS) {
        n = RP_MAX_SLOTS;
    }
    for (int i = 0; i < n; i++) {
        slots[i] = va_arg(ap, uintptr_t);
    }
    va_end(ap);
    return rp_va_unicode_from_format(format, slots, n);
}

/* PyRun_SimpleString (Python/pythonrun.c): execute a C string as Python
 * code. Defined in C (dllexport) and delegates to the Rust implementation. */
extern int rp_va_run_simple_string(const char *code);

RP_EXPORT int PyRun_SimpleString(const char *code) {
    return rp_va_run_simple_string(code);
}

/* PyRun_String (Python/pythonrun.c): execute code with scope. */
extern void *rp_va_run_string(const char *code, int start, void *globals, void *locals);

RP_EXPORT void *PyRun_String(const char *code, int start, void *globals, void *locals) {
    return rp_va_run_string(code, start, globals, locals);
}

/* PySys_* (Python/sysmodule.c) */
extern void *rp_va_sys_get_object(const char *name);
extern int rp_va_sys_set_object(const char *name, void *value);
extern void rp_va_sys_set_path(const char *path);

RP_EXPORT void *PySys_GetObject(const char *name) {
    return rp_va_sys_get_object(name);
}

RP_EXPORT int PySys_SetObject(const char *name, void *value) {
    return rp_va_sys_set_object(name, value);
}

RP_EXPORT void PySys_SetPath(const char *path) {
    rp_va_sys_set_path(path);
}

/* Py_Exit (Python/sysmodule.c) */
extern void rp_va_exit(int status);

RP_EXPORT void Py_Exit(int status) {
    rp_va_exit(status);
}

/* PyModule_AddObject (Objects/moduleobject.c) — steal reference variant. */
extern int rp_va_module_add_object(void *module, const char *name, void *value);

RP_EXPORT int PyModule_AddObject(void *module, const char *name, void *value) {
    return rp_va_module_add_object(module, name, value);
}

/* PySys_WriteStdout / PySys_WriteStderr (Python/sysmodule.c) — printf-style
 * output to sys.stdout/sys.stderr. Variadic, so the C shim captures the args. */
extern int rp_va_sys_write_stdout(const char *format, const uintptr_t *slots, int nslots);
extern int rp_va_sys_write_stderr(const char *format, const uintptr_t *slots, int nslots);

RP_EXPORT int PySys_WriteStdout(const char *format, ...) {
    va_list ap;
    uintptr_t slots[RP_MAX_SLOTS];
    int n;
    va_start(ap, format);
    n = rp_count_printf_slots(format == NULL ? "" : format);
    if (n > RP_MAX_SLOTS) { n = RP_MAX_SLOTS; }
    for (int i = 0; i < n; i++) { slots[i] = va_arg(ap, uintptr_t); }
    va_end(ap);
    return rp_va_sys_write_stdout(format, slots, n);
}

RP_EXPORT int PySys_WriteStderr(const char *format, ...) {
    va_list ap;
    uintptr_t slots[RP_MAX_SLOTS];
    int n;
    va_start(ap, format);
    n = rp_count_printf_slots(format == NULL ? "" : format);
    if (n > RP_MAX_SLOTS) { n = RP_MAX_SLOTS; }
    for (int i = 0; i < n; i++) { slots[i] = va_arg(ap, uintptr_t); }
    va_end(ap);
    return rp_va_sys_write_stderr(format, slots, n);
}

/* PyInstanceMethod_New (Objects/classobject.c) — wrap a callable as an
 * instance method. Defined in C (dllexport) to survive linker stripping. */
extern void *rp_va_instancemethod_new(void *func);

RP_EXPORT void *PyInstanceMethod_New(void *func) {
    return rp_va_instancemethod_new(func);
}

/* PyMemoryView_FromMemory (Objects/memoryobject.c) */
extern void *rp_va_memoryview_from_memory(const char *data, intptr_t size, const char *format);

RP_EXPORT void *PyMemoryView_FromMemory(const char *data, intptr_t size, const char *format) {
    return rp_va_memoryview_from_memory(data, size, format);
}

/* PyUnicode_Substring (Objects/unicodeobject.c) */
extern void *rp_va_unicode_substring(void *obj, intptr_t start, intptr_t end);

RP_EXPORT void *PyUnicode_Substring(void *obj, intptr_t start, intptr_t end) {
    return rp_va_unicode_substring(obj, start, end);
}

/* PyUnicode_Split (Objects/unicodeobject.c) — split a string by a separator. */
extern void *rp_va_unicode_split(void *obj, void *sep, intptr_t maxsplit);

RP_EXPORT void *PyUnicode_Split(void *obj, void *sep, intptr_t maxsplit) {
    return rp_va_unicode_split(obj, sep, maxsplit);
}

/* PyUnicode_Replace (Objects/unicodeobject.c) — replace substrings. */
extern void *rp_va_unicode_replace(void *obj, void *old, void *new_, intptr_t maxreplace);

RP_EXPORT void *PyUnicode_Replace(void *obj, void *old, void *new_, intptr_t maxreplace) {
    return rp_va_unicode_replace(obj, old, new_, maxreplace);
}

/* Py_GetProgramName/Py_SetProgramName (Python/pylifecycle.c) */
extern const char *rp_va_get_program_name(void);
extern void rp_va_set_program_name(const char *name);
extern const char *rp_va_get_prefix(void);
extern const char *rp_va_get_exec_prefix(void);
extern const char *rp_va_get_path(void);

RP_EXPORT const char *Py_GetProgramName(void) {
    return rp_va_get_program_name();
}

RP_EXPORT void Py_SetProgramName(const char *name) {
    rp_va_set_program_name(name);
}

RP_EXPORT const char *Py_GetPrefix(void) {
    return rp_va_get_prefix();
}

RP_EXPORT const char *Py_GetExecPrefix(void) {
    return rp_va_get_exec_prefix();
}

RP_EXPORT const char *Py_GetPath(void) {
    return rp_va_get_path();
}

/* Py_AtExit (Python/pylifecycle.c) — register a cleanup callback. */
extern int rp_va_atexit(void (*func)(void));

RP_EXPORT int Py_AtExit(void (*func)(void)) {
    return rp_va_atexit(func);
}

/* PyBytes_FromFormat (Objects/bytesobject.c) — create bytes from printf format. */
extern void *rp_va_bytes_from_format(const char *format, const uintptr_t *slots, int nslots);

RP_EXPORT void *PyBytes_FromFormat(const char *format, ...) {
    va_list ap;
    uintptr_t slots[RP_MAX_SLOTS];
    int n;
    va_start(ap, format);
    n = rp_count_printf_slots(format == NULL ? "" : format);
    if (n > RP_MAX_SLOTS) { n = RP_MAX_SLOTS; }
    for (int i = 0; i < n; i++) { slots[i] = va_arg(ap, uintptr_t); }
    va_end(ap);
    return rp_va_bytes_from_format(format, slots, n);
}

/* PyTuple_Pack (Objects/tupleobject.c): build a tuple from n object pointers.
 * The Rust implementation transfers ownership of the item references. */
void *rp_va_tuple_pack(const uintptr_t *slots, int nslots);

RP_EXPORT void *PyTuple_Pack(intptr_t n, ...) {
    va_list ap;
    uintptr_t slots[RP_MAX_SLOTS];
    int i;
    int count = (int)n;
    va_start(ap, n);
    for (i = 0; i < count && i < RP_MAX_SLOTS; i++) {
        slots[i] = va_arg(ap, uintptr_t);
    }
    va_end(ap);
    return rp_va_tuple_pack(slots, count);
}
