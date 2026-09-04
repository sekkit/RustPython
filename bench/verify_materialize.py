# Verify foreign (CPython-layout) list dual-path API:
#   PyList_NewForeign(n) -> foreign buffer
#   simulate numpy's inlined PyList_SET_ITEM: ob_item[i] = item
#   PyList_Size / PyList_GetItemRef must read back through the foreign branch.
import ctypes
import os

dll_path = os.path.join(os.path.dirname(__file__), "..", "target", "release", "rustpythonapi.dll")
dll = ctypes.CDLL(os.path.abspath(dll_path))

dll.PyList_NewForeign.restype = ctypes.c_void_p
dll.PyList_NewForeign.argtypes = [ctypes.c_ssize_t]
dll.PyUnicode_FromString.restype = ctypes.c_void_p
dll.PyUnicode_FromString.argtypes = [ctypes.c_char_p]
dll.PyList_Size.restype = ctypes.c_ssize_t
dll.PyList_Size.argtypes = [ctypes.c_void_p]
dll.PyList_GetItemRef.restype = ctypes.c_void_p
dll.PyList_GetItemRef.argtypes = [ctypes.c_void_p, ctypes.c_ssize_t]

# 1. Create a CPython-layout list buffer.
lst = dll.PyList_NewForeign(1)
assert lst, "PyList_NewForeign returned NULL"
print(f"foreign list = {lst:#x}")

# 2. Simulate numpy inline macro: ob_item[0] = unicode.
s = dll.PyUnicode_FromString(b"numpy-item")
assert s, "PyUnicode_FromString failed"
print(f"unicode item = {s:#x}")

ob_item_ptr = ctypes.cast(lst + 0x18, ctypes.POINTER(ctypes.c_void_p)).contents.value
print(f"ob_item array = {ob_item_ptr:#x}")
ctypes.cast(ob_item_ptr, ctypes.POINTER(ctypes.c_void_p))[0] = s
print("inline ob_item[0] write OK (no AV)")

# 3. Foreign dual path: PyList_Size reads ob_size at 0x10.
n = dll.PyList_Size(lst)
print(f"PyList_Size = {n}")
assert n == 1, f"expected 1, got {n}"

# 4. PyList_GetItemRef reads ob_item out of the foreign buffer.
item = dll.PyList_GetItemRef(lst, 0)
print(f"PyList_GetItemRef = {item:#x}")
assert item == s, f"item mismatch {item:#x} vs {s:#x}"

# 5. Out-of-range index must raise IndexError, not crash.
try:
    dll.PyList_GetItemRef(lst, 5)
    print("OOB did NOT raise")
except Exception as e:
    print(f"OOB raised {type(e).__name__} (expected IndexError)")

print("FOREIGN_LIST_DUAL_PATH_OK")