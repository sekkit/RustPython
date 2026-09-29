import gc
import weakref


class C:
    def method(self):
        return None


obj = C()
obj_ref = weakref.ref(obj)

# A temporary bound method is cached by the VM's freelist after this expression.
obj.method

del obj
gc.collect()
assert obj_ref() is None

print("ok")
