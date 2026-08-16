"""Pure-Python _lsprof compatibility shim for RustPython.

RustPython has no C _lsprof module; this module implements the subset of
its API used by the profiling package (and the legacy cProfile) on top of
sys.monitoring, mirroring CPython's Modules/_lsprof.c.

The profiler registers itself as sys.monitoring tool PROFILER_ID ("cProfile")
and records PY_START/PY_RESUME/PY_THROW/PY_RETURN/PY_YIELD/PY_UNWIND plus
CALL/C_RETURN/C_RAISE events.  Timing semantics follow CPython's _lsprof:
totaltime includes subcalls, inlinetime excludes them.
"""

import operator
import sys
import time

# (event, callback-method) table, mirroring callback_table in _lsprof.c
_EVENT_CALLBACKS = (
    (sys.monitoring.events.PY_START, "_pystart_callback"),
    (sys.monitoring.events.PY_RESUME, "_pystart_callback"),
    (sys.monitoring.events.PY_THROW, "_pythrow_callback"),
    (sys.monitoring.events.PY_RETURN, "_pyreturn_callback"),
    (sys.monitoring.events.PY_YIELD, "_pyreturn_callback"),
    (sys.monitoring.events.PY_UNWIND, "_pyreturn_callback"),
    (sys.monitoring.events.CALL, "_ccall_callback"),
    (sys.monitoring.events.C_RETURN, "_creturn_callback"),
    (sys.monitoring.events.C_RAISE, "_creturn_callback"),
)


class _Unraisable:
    """Minimal stand-in for sys.UnraisableHookArgs."""

    __slots__ = ("exc_type", "exc_value", "exc_traceback", "err_msg", "object")

    def __init__(self, exc_type, exc_value, exc_traceback, err_msg, object):
        self.exc_type = exc_type
        self.exc_value = exc_value
        self.exc_traceback = exc_traceback
        self.err_msg = err_msg
        self.object = object


class _Entry:
    """ProfilerEntry equivalent: stats for one function."""

    __slots__ = (
        "key",
        "user_obj",
        "tt",
        "it",
        "callcount",
        "reccallcount",
        "recursion_level",
        "calls",
    )

    def __init__(self, key, user_obj):
        self.key = key
        self.user_obj = user_obj
        self.tt = 0.0
        self.it = 0.0
        self.callcount = 0
        self.reccallcount = 0
        self.recursion_level = 0
        self.calls = {}


class _SubEntry:
    """ProfilerSubEntry equivalent: stats of a call in a caller's entry."""

    __slots__ = ("entry", "tt", "it", "callcount", "reccallcount", "recursion_level")

    def __init__(self, entry):
        self.entry = entry
        self.tt = 0.0
        self.it = 0.0
        self.callcount = 0
        self.reccallcount = 0
        self.recursion_level = 0


class _Context:
    """ProfilerContext equivalent: an active (unreturned) call."""

    __slots__ = ("t0", "subt", "previous", "entry")

    def __init__(self):
        self.t0 = 0.0
        self.subt = 0.0
        self.previous = None
        self.entry = None


class ProfilerEntry:
    """Legacy name kept for compatibility with the previous shim."""

    __slots__ = ("code", "callcount", "reccallcount", "inlinetime", "totaltime", "calls")

    def __init__(self, code):
        self.code = code
        self.callcount = 0
        self.reccallcount = 0
        self.inlinetime = 0.0
        self.totaltime = 0.0
        self.calls = {}


class ProfilerStatsEntry:
    __slots__ = ("code", "callcount", "reccallcount", "inlinetime", "totaltime", "calls")

    def __init__(self, code, callcount, reccallcount, inlinetime, totaltime, calls):
        self.code = code
        self.callcount = callcount
        self.reccallcount = reccallcount
        self.inlinetime = inlinetime
        self.totaltime = totaltime
        self.calls = calls


class Profiler:
    """Profiler(timer=None, timeunit=0.0, subcalls=True, builtins=True)"""

    def __init__(self, timer=None, timeunit=0.0, subcalls=True, builtins=True):
        self._timer = timer
        self._timeunit = float(timeunit)
        self._subcalls = bool(subcalls)
        self._builtins = bool(builtins)
        self._tool_id = sys.monitoring.PROFILER_ID
        self._missing = sys.monitoring.MISSING
        self._enabled = False
        self._in_timer = False
        self._entries = {}
        self._current = None
        self._free_contexts = []

    # -- external timer support (POF_EXT_TIMER / CallExternalTimer) ---------

    def _call_timer(self):
        timer = self._timer
        if timer is None:
            return time.perf_counter_ns() * 1e-9
        self._in_timer = True
        try:
            try:
                value = timer()
            except BaseException:
                self._write_unraisable(
                    "Exception ignored while calling _lsprof timer", timer
                )
                return 0.0
        finally:
            self._in_timer = False
        try:
            if self._timeunit > 0.0:
                # Interpret the result as an integer that will be scaled
                # (like _PyTime_FromLong).
                return float(operator.index(value)) * self._timeunit
            # Interpret the result as seconds (like _PyTime_FromSecondsObject).
            if isinstance(value, int):
                return float(value)
            if isinstance(value, float):
                return value
            raise TypeError("timer returned a non-numeric value")
        except (TypeError, ValueError, OverflowError):
            self._write_unraisable(
                "Exception ignored while calling _lsprof timer", timer
            )
            return 0.0

    def _write_unraisable(self, err_msg, obj):
        exc_type, exc_value, exc_traceback = sys.exc_info()
        args = _Unraisable(exc_type, exc_value, exc_traceback, err_msg, obj)
        hook = getattr(sys, "unraisablehook", None)
        if hook is None:
            hook = getattr(sys, "__unraisablehook__", None)
        if hook is not None:
            try:
                hook(args)
            except Exception:
                pass

    # -- context management (initContext / Stop / ptrace_*) ------------------

    def _pop_free_context(self):
        if self._free_contexts:
            return self._free_contexts.pop()
        return _Context()

    def _enter_call(self, key, user_obj):
        entry = self._entries.get(key)
        if entry is None:
            entry = _Entry(key, user_obj)
            self._entries[key] = entry
        ctx = self._pop_free_context()
        ctx.entry = entry
        ctx.subt = 0.0
        ctx.previous = self._current
        self._current = ctx
        entry.recursion_level += 1
        if self._subcalls and ctx.previous is not None:
            caller = ctx.previous.entry
            sub = caller.calls.get(key)
            if sub is None:
                sub = _SubEntry(entry)
                caller.calls[key] = sub
            sub.recursion_level += 1
        ctx.t0 = self._call_timer()

    def _stop(self, ctx, entry):
        tt = self._call_timer() - ctx.t0
        it = tt - ctx.subt
        if ctx.previous is not None:
            ctx.previous.subt += tt
        self._current = ctx.previous
        entry.recursion_level -= 1
        if entry.recursion_level == 0:
            entry.tt += tt
        else:
            entry.reccallcount += 1
        entry.it += it
        entry.callcount += 1
        if self._subcalls and ctx.previous is not None:
            caller = ctx.previous.entry
            sub = caller.calls.get(entry.key)
            if sub is not None:
                sub.recursion_level -= 1
                if sub.recursion_level == 0:
                    sub.tt += tt
                else:
                    sub.reccallcount += 1
                sub.it += it
                sub.callcount += 1

    def _leave_call(self, key):
        ctx = self._current
        if ctx is None:
            return
        entry = self._entries.get(key)
        if entry is not None:
            self._stop(ctx, entry)
        else:
            self._current = ctx.previous
        ctx.previous = None
        self._free_contexts.append(ctx)

    def _flush_unmatched(self):
        while self._current is not None:
            ctx = self._current
            entry = ctx.entry
            if entry is not None:
                self._stop(ctx, entry)
            else:
                self._current = ctx.previous
            # contexts are discarded here, not recycled

    # -- C-callable identification (get_cfunc_from_callable) ------------------

    def _get_cfunc(self, callable, self_arg):
        # Returns a builtin function/method to profile, or None.
        type_name = type(callable).__name__
        if type_name == "builtin_function_or_method":
            return callable
        if type_name == "method_descriptor":
            if self_arg is self._missing:
                return None
            try:
                meth = callable.__get__(self_arg)
            except Exception:
                return None
            if type(meth).__name__ == "builtin_function_or_method":
                return meth
            return None
        if type_name in ("function", "method"):
            # CPython's Profiler methods are C methods (method descriptors),
            # so enable()/disable() surface as built-in methods in the profile
            # output; record ours the same way.  Method calls arrive either as
            # (function, self) or as a bound method object.
            if type_name == "method":
                try:
                    self_obj = callable.__self__
                except AttributeError:
                    return None
                if isinstance(self_obj, Profiler):
                    return callable
                return None
            if isinstance(self_arg, Profiler):
                return callable
            return None
        return None

    def _cfunc_key(self, cfunc, self_arg=None):
        # CPython keys C entries by the (shared) method definition; derive an
        # equivalent stable key for the builtin function or bound method.
        name = cfunc.__name__
        try:
            self_obj = cfunc.__self__
        except AttributeError:
            self_obj = self_arg
        if self_obj is None:
            return ("builtin", getattr(cfunc, "__module__", None), name)
        return ("builtin-method", id(type(self_obj)), name)

    def _normalize_user_obj(self, obj, self_arg=None):
        # normalizeUserObj in _lsprof.c: replace builtin functions/methods
        # with a descriptive string.  CPython binds module-level builtins to
        # their module, so the label always takes the 'built-in method' form.
        name = obj.__name__
        module = getattr(obj, "__module__", None)
        try:
            self_obj = obj.__self__
        except AttributeError:
            self_obj = self_arg
        if self_obj is not None and isinstance(self_obj, Profiler):
            # CPython's Profiler methods are C methods; their label is the
            # repr of the method descriptor on the _lsprof.Profiler type.
            return "<method '%s' of '_lsprof.Profiler' objects>" % name
        if self_obj is None:
            if module:
                return "<built-in method %s.%s>" % (module, name)
            return "<built-in method %s>" % name
        mo = getattr(type(self_obj), name, None)
        if mo is not None:
            return repr(mo)
        if module:
            return "<built-in method %s.%s>" % (module, name)
        return "<built-in method %s>" % name

    # -- monitoring callbacks ------------------------------------------------

    def _pystart_callback(self, code, instruction_offset):
        self._enter_call(id(code), code)

    def _pythrow_callback(self, code, instruction_offset, exception):
        self._enter_call(id(code), code)

    def _pyreturn_callback(self, code, instruction_offset, retval):
        self._leave_call(id(code))

    def _ccall_callback(self, code, instruction_offset, callable, self_arg):
        if not self._enabled:
            return
        if self._builtins:
            cfunc = self._get_cfunc(callable, self_arg)
            if cfunc is not None:
                # normalizeUserObj: replace the function with a descriptive
                # string (the string is only stored when the entry is created)
                self._enter_call(
                    self._cfunc_key(cfunc, self_arg),
                    self._normalize_user_obj(cfunc, self_arg),
                )

    def _creturn_callback(self, code, instruction_offset, callable, self_arg):
        if not self._enabled:
            return
        if self._builtins:
            cfunc = self._get_cfunc(callable, self_arg)
            if cfunc is not None:
                self._leave_call(self._cfunc_key(cfunc, self_arg))

    # -- public API -----------------------------------------------------------

    def enable(self, subcalls=True, builtins=True):
        self._subcalls = bool(subcalls)
        self._builtins = bool(builtins)
        # Initialize the timing context; a failing external timer surfaces
        # here as an unraisable exception (matching CPython's initContext).
        # Runs before the tool is registered, so it is never profiled.
        self._call_timer()
        monitoring = sys.monitoring
        monitoring.use_tool_id(self._tool_id, "cProfile")
        all_events = 0
        for event, method_name in _EVENT_CALLBACKS:
            callback = getattr(self, method_name)
            monitoring.register_callback(self._tool_id, event, callback)
            all_events |= event
        monitoring.set_events(self._tool_id, all_events)
        self._enabled = True

    def disable(self):
        if self._in_timer:
            raise RuntimeError("cannot disable profiler in external timer")
        if self._enabled:
            # Unset first so the monitoring calls below (register_callback /
            # set_events / free_tool_id) are not profiled themselves.
            self._enabled = False
            monitoring = sys.monitoring
            for event, _ in _EVENT_CALLBACKS:
                monitoring.register_callback(self._tool_id, event, None)
            monitoring.set_events(self._tool_id, 0)
            monitoring.free_tool_id(self._tool_id)
            self._flush_unmatched()

    def clear(self):
        if self._in_timer:
            raise RuntimeError("cannot clear profiler in external timer")
        self._entries.clear()
        self._current = None
        self._free_contexts.clear()

    def getstats(self):
        # Returns entries in CPython order: insertion order of first call
        entries = []
        for entry in self._entries.values():
            if entry.callcount == 0:
                continue
            calls = []
            for sub in entry.calls.values():
                calls.append(
                    ProfilerStatsEntry(
                        sub.entry.user_obj,
                        sub.callcount,
                        sub.reccallcount,
                        sub.it,
                        sub.tt,
                        [],
                    )
                )
            entries.append(
                ProfilerStatsEntry(
                    entry.user_obj,
                    entry.callcount,
                    entry.reccallcount,
                    entry.it,
                    entry.tt,
                    calls,
                )
            )
        return entries
