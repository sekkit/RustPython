use crate::{
    AsObject,
    builtins::{descriptor::PyMethodDescriptor, PyBoundMethod, PyFunction},
    function::{FuncArgs, IntoFuncArgs},
    types::{GenericMethod, VectorCallFunc},
    {PyObject, PyObjectRef, PyResult, VirtualMachine},
};

impl PyObject {
    #[inline]
    #[must_use]
    pub fn to_callable(&self) -> Option<PyCallable<'_>> {
        PyCallable::new(self)
    }

    #[inline]
    #[must_use]
    pub fn is_callable(&self) -> bool {
        self.to_callable().is_some()
    }

    /// PyObject_Call*Arg* series
    #[inline]
    pub fn call(&self, args: impl IntoFuncArgs, vm: &VirtualMachine) -> PyResult {
        let args = args.into_args(vm);
        self.call_with_args(args, vm)
    }

    /// PyObject_Call
    pub fn call_with_args(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
        let Some(callable) = self.to_callable() else {
            return Err(
                vm.new_type_error(format!("'{}' object is not callable", self.class().name()))
            );
        };
        vm_trace!("Invoke: {:?} {:?}", callable, args);
        callable.invoke(args, vm)
    }

    /// Vectorcall: call with owned positional args + optional kwnames.
    /// Falls back to FuncArgs-based call if no vectorcall slot.
    #[inline]
    pub fn vectorcall(
        &self,
        args: Vec<PyObjectRef>,
        nargs: usize,
        kwnames: Option<&[PyObjectRef]>,
        vm: &VirtualMachine,
    ) -> PyResult {
        let Some(callable) = self.to_callable() else {
            return Err(
                vm.new_type_error(format!("'{}' object is not callable", self.class().name()))
            );
        };
        callable.invoke_vectorcall(args, nargs, kwnames, vm)
    }
}

#[derive(Debug)]
pub struct PyCallable<'a> {
    pub obj: &'a PyObject,
    pub call: GenericMethod,
    pub vectorcall: Option<VectorCallFunc>,
}

impl<'a> PyCallable<'a> {
    pub fn new(obj: &'a PyObject) -> Option<Self> {
        let slots = &obj.class().slots;
        let call = slots.call.load()?;
        let vectorcall = slots.vectorcall.load();
        Some(PyCallable {
            obj,
            call,
            vectorcall,
        })
    }

    pub fn invoke(&self, args: impl IntoFuncArgs, vm: &VirtualMachine) -> PyResult {
        let args = args.into_args(vm);
        if !vm.use_tracing.get() {
            return (self.call)(self.obj, args, vm);
        }
        // Python functions get 'call'/'return' events from with_frame().
        // Bound methods delegate to the inner callable, which fires its own events.
        // All other callables only get c_call/c_return/c_exception when they are
        // built-in functions/methods or method descriptors, matching CPython's
        // trace_call_function in ceval.c (types, partials, ... are called
        // without tracing events).
        let Some(trace_callable) = self.trace_c_callable(vm, args.args.first()) else {
            return (self.call)(self.obj, args, vm);
        };
        vm.trace_event(TraceEvent::CCall, Some(trace_callable.clone()))?;
        let result = (self.call)(self.obj, args, vm);
        if result.is_ok() {
            vm.trace_event(TraceEvent::CReturn, Some(trace_callable))?;
        } else {
            let _ = vm.trace_event(TraceEvent::CException, Some(trace_callable));
        }
        result
    }

    /// Vectorcall dispatch: use vectorcall slot if available, else fall back to FuncArgs.
    #[inline]
    pub fn invoke_vectorcall(
        &self,
        args: Vec<PyObjectRef>,
        nargs: usize,
        kwnames: Option<&[PyObjectRef]>,
        vm: &VirtualMachine,
    ) -> PyResult {
        if let Some(vc) = self.vectorcall {
            if !vm.use_tracing.get() {
                return vc(self.obj, args, nargs, kwnames, vm);
            }
            let Some(trace_callable) = self.trace_c_callable(vm, args.first()) else {
                return vc(self.obj, args, nargs, kwnames, vm);
            };
            vm.trace_event(TraceEvent::CCall, Some(trace_callable.clone()))?;
            let result = vc(self.obj, args, nargs, kwnames, vm);
            if result.is_ok() {
                vm.trace_event(TraceEvent::CReturn, Some(trace_callable))?;
            } else {
                let _ = vm.trace_event(TraceEvent::CException, Some(trace_callable));
            }
            result
        } else {
            // Fallback: convert owned Vec to FuncArgs (move, no clone)
            let func_args = FuncArgs::from_vectorcall_owned(args, nargs, kwnames);
            self.invoke(func_args, vm)
        }
    }

    /// The object passed to c_call/c_return/c_exception trace events, or None
    /// when the callable does not receive them (CPython trace_call_function
    /// parity: only PyCFunction/PyCMethod and method descriptors, the latter
    /// bound with the first argument, are traced).
    fn trace_c_callable(
        &self,
        vm: &VirtualMachine,
        first_arg: Option<&PyObjectRef>,
    ) -> Option<PyObjectRef> {
        if self.obj.downcast_ref::<PyFunction>().is_some()
            || self.obj.downcast_ref::<PyBoundMethod>().is_some()
        {
            return None;
        }
        if self.obj.class().is(vm.ctx.types.builtin_function_or_method_type) {
            return Some(self.obj.to_owned());
        }
        if self.obj.class().is(vm.ctx.types.method_descriptor_type) {
            // CPython creates a temporary bound method as the trace argument.
            // Without a first argument the call itself would raise TypeError,
            // so no profiling either.
            if let Some(descr) = self.obj.downcast_ref::<PyMethodDescriptor>()
                && let Some(self_arg) = first_arg
            {
                return Some(descr.bind(self_arg.clone(), &vm.ctx).into());
            }
            return None;
        }
        None
    }
}

/// Trace events for sys.settrace and sys.setprofile.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum TraceEvent {
    Call,
    Return,
    Exception,
    Line,
    Opcode,
    CCall,
    CReturn,
    CException,
}

impl TraceEvent {
    /// Whether sys.settrace receives this event.
    #[must_use]
    const fn is_trace_event(self) -> bool {
        matches!(
            self,
            Self::Call | Self::Return | Self::Exception | Self::Line | Self::Opcode
        )
    }

    /// Whether sys.setprofile receives this event.
    /// In legacy_tracing.c, profile callbacks are only registered for
    /// PY_RETURN, PY_UNWIND, C_CALL, C_RETURN, C_RAISE.
    #[must_use]
    const fn is_profile_event(self) -> bool {
        matches!(
            self,
            Self::Call | Self::Return | Self::CCall | Self::CReturn | Self::CException
        )
    }

    /// Whether this event is dispatched only when f_trace_opcodes is set.
    #[must_use]
    pub(crate) const fn is_opcode_event(self) -> bool {
        matches!(self, Self::Opcode)
    }
}

impl core::fmt::Display for TraceEvent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Call => write!(f, "call"),
            Self::Return => write!(f, "return"),
            Self::Exception => write!(f, "exception"),
            Self::Line => write!(f, "line"),
            Self::Opcode => write!(f, "opcode"),
            Self::CCall => write!(f, "c_call"),
            Self::CReturn => write!(f, "c_return"),
            Self::CException => write!(f, "c_exception"),
        }
    }
}

impl VirtualMachine {
    /// Call registered trace function.
    ///
    /// Returns the trace function's return value:
    /// - `Some(obj)` if the trace function returned a non-None value
    /// - `None` if it returned Python None or no trace function was active
    ///
    /// In CPython's trace protocol:
    /// - For 'call' events: the return value determines the per-frame `f_trace`
    /// - For 'line'/'return' events: the return value can update `f_trace`
    #[inline]
    pub(crate) fn trace_event(
        &self,
        event: TraceEvent,
        arg: Option<PyObjectRef>,
    ) -> PyResult<Option<PyObjectRef>> {
        if self.use_tracing.get() && !self.tracing_is_suppressed() {
            self._trace_event_inner(event, arg)
        } else {
            Ok(None)
        }
    }
    fn _trace_event_inner(
        &self,
        event: TraceEvent,
        arg: Option<PyObjectRef>,
    ) -> PyResult<Option<PyObjectRef>> {
        let trace_func = self.trace_func.borrow().to_owned();
        let profile_func = self.profile_func.borrow().to_owned();
        if self.is_none(&trace_func) && self.is_none(&profile_func) {
            return Ok(None);
        }

        let is_trace_event = event.is_trace_event();
        let is_profile_event = event.is_profile_event();
        let is_opcode_event = event.is_opcode_event();

        let Some(frame_ref) = crate::frame::current_thread_frame_materialize(self) else {
            return Ok(None);
        };

        // Opcode events are only dispatched when f_trace_opcodes is set.
        if is_opcode_event
            && !frame_ref
                .iframe()
                .cold_opt()
                .is_some_and(|c| *c.trace_opcodes.lock())
        {
            return Ok(None);
        }

        let frame: PyObjectRef = frame_ref.into();
        let event = self.ctx.new_str(event.to_string()).into();
        let args = vec![frame, event, arg.unwrap_or_else(|| self.ctx.none())];

        let mut trace_result = None;

        // temporarily disable tracing, during the call to the
        // tracing function itself.
        if is_trace_event && !self.is_none(&trace_func) {
            self.use_tracing.set(false);
            self.enter_tracing();
            let res = trace_func.call(args.clone(), self);
            self.leave_tracing();
            self.use_tracing.set(true);
            match res {
                Ok(result) => {
                    if !self.is_none(&result) {
                        trace_result = Some(result);
                    }
                }
                Err(e) => {
                    // trace_trampoline behavior: clear per-frame f_trace
                    // and propagate the error.
                    if let Some(frame_ref) = self.current_frame() {
                        *frame_ref.iframe().cold().trace.lock() = None;
                    }
                    return Err(e);
                }
            }
        }

        if is_profile_event && !self.is_none(&profile_func) {
            self.use_tracing.set(false);
            self.enter_tracing();
            let res = profile_func.call(args, self);
            self.leave_tracing();
            self.use_tracing.set(true);
            if res.is_err() {
                *self.profile_func.borrow_mut() = self.ctx.none();
            }
        }
        Ok(trace_result)
    }
}
