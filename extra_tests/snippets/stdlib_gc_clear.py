import gc
import weakref


class Node:
    pass


target = Node()
referrer = Node()
referrer.target = target
assert referrer.__dict__ in gc.get_referrers(target)


def make_filter_cycle():
    node = Node()
    items = [node]
    node.filter = filter(None, items)
    return weakref.ref(node)


def make_exception_cycle():
    node = Node()
    error = RuntimeError(node)
    node.error = error
    return weakref.ref(node)


def make_slice_cycle():
    node = Node()
    node.slice = slice(None, node, None)
    return weakref.ref(node)


for make_cycle in (make_filter_cycle, make_exception_cycle, make_slice_cycle):
    gc.collect()
    ref = make_cycle()
    gc.collect()
    assert ref() is None


resurrected = []
finalizer_calls = []


class DirectResurrection:
    def __init__(self):
        self.loop = self

    def __del__(self):
        finalizer_calls.append(id(self))
        resurrected.append(self)


gc.collect()
obj = DirectResurrection()
obj_ref = weakref.ref(obj)
del obj
gc.collect()
assert len(resurrected) == 1
assert resurrected[0].loop is resurrected[0]
assert len(finalizer_calls) == 1
resurrected.clear()
gc.collect()
assert obj_ref() is None
assert len(finalizer_calls) == 1


class Parent:
    def __init__(self):
        self.child = None
        self.loop = self

    def __del__(self):
        resurrected.append(self)


class Child:
    def __init__(self):
        self.parent = None


gc.collect()
parent = Parent()
child = Child()
parent.child = child
child.parent = parent
del parent, child
gc.collect()
assert len(resurrected) == 1
assert isinstance(resurrected[0].child, Child)
assert resurrected[0].child.parent is resurrected[0]
resurrected.clear()
gc.collect()


saved_by_callback = []


class CallbackTrigger:
    pass


class CallbackVictim:
    def __init__(self):
        self.loop = self


def save_victim(_):
    for candidate in gc.get_objects():
        if isinstance(candidate, CallbackVictim):
            saved_by_callback.append(candidate)
            return


def make_callback_graph():
    trigger = CallbackTrigger()
    trigger.loop = trigger
    victim = CallbackVictim()
    trigger_ref = weakref.ref(trigger, save_victim)
    return trigger_ref


gc.collect()
trigger_ref = make_callback_graph()
gc.collect()
assert trigger_ref() is None
assert len(saved_by_callback) == 1
assert saved_by_callback[0].loop is saved_by_callback[0]
saved_by_callback.clear()
gc.collect()

print("ok")
