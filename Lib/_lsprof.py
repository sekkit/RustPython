"""Pure-Python _lsprof compatibility shim for RustPython.

RustPython has no C _lsprof module; this module implements the subset of
its API used by the profiling package (and the legacy cProfile) on top of
sys.setprofile. Timing semantics follow CPython's _lsprof: totaltime
includes subcalls, inlinetime excludes them.
"""

import sys
import time


class ProfilerEntry:
    __slots__ = ("code", "callcount", "reccallcount", "inlinetime", "totaltime", "calls")

    def __init__(self, code):
        self.code = code
        self.callcount = 0
        self.reccallcount = 0
        self.inlinetime = 0.0
        self.totaltime = 0.0
        self.calls = {}


class Profiler:
    """Profiler(timer=None, timeunit=None, subcalls=True, builtins=True)"""

    def __init__(self, timer=None, timeunit=None, subcalls=True, builtins=True):
        self._timer = timer or time.perf_counter
        self._timeunit = timeunit if timeunit is not None else 1.0
        self._entries = {}
        self._stack = []
        self._enabled = False
        self._saved_profile = None

    def enable(self):
        if self._enabled:
            return
        self._enabled = True
        self._saved_profile = sys.getprofile()
        sys.setprofile(self._trace)

    def disable(self):
        if not self._enabled:
            return
        self._enabled = False
        sys.setprofile(self._saved_profile)
        self._saved_profile = None

    def _trace(self, frame, event, arg):
        code = frame.f_code
        t = self._timer()
        if event == "call":
            entry = self._entries.get(code)
            if entry is None:
                entry = ProfilerEntry(code)
                self._entries[code] = entry
            entry.callcount += 1
            if self._stack and self._stack[-1][0] is code:
                entry.reccallcount += 1
            if self._stack:
                parent_code, parent_entry, _ = self._stack[-1]
                if parent_code is not code:
                    sub = parent_entry.calls.get(code)
                    if sub is None:
                        sub = ProfilerEntry(code)
                        parent_entry.calls[code] = sub
                    sub.callcount += 1
            self._stack.append((code, entry, t))
        elif event == "return" and self._stack:
            code2, entry, start = self._stack.pop()
            if code2 is code:
                dt = (t - start) * self._timeunit
                entry.inlinetime += dt
                # totaltime of the frame includes its own subcalls: the
                # subcall deltas were already added to this entry while it
                # was on the stack? No - add them now from the child span.
                entry.totaltime += dt
                if self._stack:
                    # Parent's totaltime includes this call's span.
                    self._stack[-1][1].totaltime += dt

    def getstats(self):
        # Returns entries in CPython order: insertion order of first call
        entries = []
        for entry in self._entries.values():
            calls = list(entry.calls.values())
            entries.append(
                ProfilerStatsEntry(
                    entry.code,
                    entry.callcount,
                    entry.reccallcount,
                    entry.inlinetime,
                    entry.totaltime,
                    calls,
                )
            )
        return entries


class ProfilerStatsEntry:
    __slots__ = ("code", "callcount", "reccallcount", "inlinetime", "totaltime", "calls")

    def __init__(self, code, callcount, reccallcount, inlinetime, totaltime, calls):
        self.code = code
        self.callcount = callcount
        self.reccallcount = reccallcount
        self.inlinetime = inlinetime
        self.totaltime = totaltime
        self.calls = calls
