import sys


def append_once(target, item):
    target.append(item)


def call_bound(callable_obj, item):
    callable_obj(item)


values = []
for value in range(256):
    append_once(values, value)
assert values == list(range(256))

alias_values = []
alias = alias_values.append
for value in range(256):
    call_bound(alias, value)
assert alias_values == list(range(256))

replaced_values = []


def replacement(item):
    replaced_values.append(item)


call_bound(replacement, "replaced-callable")
assert replaced_values == ["replaced-callable"]

sink_values = []


class Sink:
    def append(self, item):
        sink_values.append(item)


sink = Sink()
append_once(sink, "generic-call-fallback")
assert sink_values == ["generic-call-fallback"]


class AppendList(list):
    def __init__(self):
        super().__init__()
        self.calls = 0

    def append(self, item):
        self.calls += 1
        super().append(item)


append_subclass = AppendList()
append_once(append_subclass, "override-fallback")
assert append_subclass == ["override-fallback"]
assert append_subclass.calls == 1

profile_events = []
profile_target = []


def profile(frame, event, arg):
    if frame.f_code.co_name == "append_once" and event in ("c_call", "c_return", "c_exception"):
        profile_events.append(event)


sys.setprofile(profile)
try:
    append_once(profile_target, "profiled")
finally:
    sys.setprofile(None)

assert profile_target == ["profiled"]
assert profile_events == ["c_call", "c_return"], profile_events
