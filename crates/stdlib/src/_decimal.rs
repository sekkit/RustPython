#[path = "_decimal_core.rs"]
mod _decimal_core;

pub(crate) use _decimal::module_def;

use std::cell::RefCell;

use malachite_bigint::BigInt;

use rustpython_common::hash;
use rustpython_common::lock::PyMutex;

use crate::vm::{
    AsObject, Py, PyObject, PyObjectRef, PyPayload, PyRef, PyResult, VirtualMachine, atomic_func,
    builtins::{
        PyComplex, PyDict, PyDictRef, PyFloat, PyInt, PyModule, PyStr, PyStrRef, PyTuple,
        PyTupleRef, PyType, PyTypeRef,
    },
    common::lock::LazyLock,
    convert::ToPyResult,
    function::{FuncArgs, OptionalArg, PyComparisonValue},
    protocol::{PyMappingMethods, PyNumberMethods, PySequenceMethods},
    types::{
        AsMapping, AsNumber, AsSequence, Comparable, Constructor, Hashable, Iterable,
        PyComparisonOp, Representable,
    },
    types::{PyTypeFlags, PyTypeSlots},
};

use self::_decimal_core::{Ctx, Dec, Rounding, Special, exc, init_exceptions, new_decimal_error};

#[pymodule]
mod _decimal {
    use super::*;
    use super::_decimal_core::{self};

    thread_local! {
        static CURRENT_CONTEXT: RefCell<Option<PyObjectRef>> = const { RefCell::new(None) };
    }
    // Process-wide default context, updatable so import_fresh_module can
    // replace it. Safe because: the VM holds a global lock during Python
    // execution (GIL), so DEFAULT_CTX is only accessed from one thread at a
    // time.
    struct SyncMutex<T>(PyMutex<T>);
    unsafe impl<T> Sync for SyncMutex<T> {}
    unsafe impl<T> Send for SyncMutex<T> {}
    impl<T> core::ops::Deref for SyncMutex<T> {
        type Target = PyMutex<T>;
        fn deref(&self) -> &Self::Target { &self.0 }
    }

    // mpdecimal's 64-bit limits (mirror the module constants set in
    // module_exec: MAX_PREC/MAX_EMAX/MIN_EMIN).
    const MAX_PREC: isize = 999_999_999_999_999_999;
    const MAX_EMAX: isize = 999_999_999_999_999_999;
    const MIN_EMIN: isize = -999_999_999_999_999_999;

    static DEFAULT_CTX: SyncMutex<Option<PyObjectRef>> = SyncMutex(PyMutex::new(None));

    // Shared DecimalTuple namedtuple class (created once per process).
    static DECIMAL_TUPLE: SyncMutex<Option<PyObjectRef>> = SyncMutex(PyMutex::new(None));

    fn get_current_context(vm: &VirtualMachine) -> PyResult<PyObjectRef> {
        if let Some(ctx) = CURRENT_CONTEXT.with(|c| c.borrow().clone()) {
            return Ok(ctx);
        }
        // A thread with no context yet gets a fresh *copy* of DefaultContext,
        // as CPython does (`getcontext() is DefaultContext` is False there).
        // Handing back the DefaultContext object itself aliased the two: flags
        // and traps mutated through one were visible through the other.
        //
        // Cache the copy: CPython's getcontext() returns the *same* current
        // context object on every call. Without caching, each getcontext()
        // handed back a distinct context, so precision/flag/trap changes made
        // through it were invisible to the arithmetic operators (which read
        // this same function), and flags raised by an operator never appeared
        // on the object the caller holds.
        let template = DEFAULT_CTX.0.lock().clone();
        let new_ctx: PyObjectRef = match template {
            Some(obj) => vm.call_method(&obj, "copy", ())?,
            None => PyContext::new_ctx(vm, 28).into_ref(&vm.ctx).into(),
        };
        set_current_context(new_ctx.clone());
        Ok(new_ctx)
    }

    fn set_current_context(ctx: PyObjectRef) {
        CURRENT_CONTEXT.with(|c| *c.borrow_mut() = Some(ctx));
    }

    fn current_ctx(vm: &VirtualMachine) -> PyResult<Ctx> {
        let obj = get_current_context(vm)?;
        let pc = obj
            .downcast_ref::<PyContext>()
            .ok_or_else(|| vm.new_type_error("context must be a Context object".to_string()))?;
        Ok(pc.ctx())
    }

    /// Read the current context's `capitals` (whether 'E' is shown for
    /// scientific exponents). Defaults to 1.
    fn current_capitals(vm: &VirtualMachine) -> PyResult<isize> {
        let obj = get_current_context(vm)?;
        let pc = obj
            .downcast_ref::<PyContext>()
            .ok_or_else(|| vm.new_type_error("context must be a Context object".to_string()))?;
        Ok(pc.capitals())
    }

    /// compare two decimals (sign*coeff*10^exp). NaN sorts before everything.
    /// Total-order payload tiebreak for NaN values (CPython compare_total):
    /// within the same NaN kind and sign, a larger payload sorts larger
    /// (NaN42 > NaN01; -NaN42 < -NaN01). Applies to compare_total_mag too
    /// (signs cleared): NaN42 > NaN01.
    fn nan_payload_cmp(a: &Dec, b: &Dec) -> i8 {
        let c = match a.coeff.cmp(&b.coeff) {
            core::cmp::Ordering::Less => -1,
            core::cmp::Ordering::Equal => 0,
            core::cmp::Ordering::Greater => 1,
        };
        if a.sign { -c } else { c }
    }

    fn dec_cmp(a: &Dec, b: &Dec) -> i8 {
        use Special::*;
        let is_nan = |d: &Dec| d.special == NaN || d.special == SignalingNaN;
        let is_inf = |d: &Dec| d.special == Infinity;
        if is_nan(a) || is_nan(b) {
            if is_nan(a) && is_nan(b) {
                return 0;
            }
            return if is_nan(a) { -1 } else { 1 };
        }
        if is_inf(a) || is_inf(b) {
            if is_inf(a) && is_inf(b) {
                if a.sign != b.sign {
                    return if a.sign { -1 } else { 1 };
                }
                return 0;
            }
            if is_inf(a) {
                return if a.sign { -1 } else { 1 };
            }
            return if b.sign { 1 } else { -1 };
        }
        // Both finite. Zeros compare equal regardless of sign.
        let az = a.is_zero();
        let bz = b.is_zero();
        if az && bz {
            return 0;
        }
        if az {
            // 0 vs nonzero: less than a positive, greater than a negative.
            return if b.sign { 1 } else { -1 };
        }
        if bz {
            return if a.sign { -1 } else { 1 };
        }
        // Both nonzero: differing signs decide immediately.
        if a.sign != b.sign {
            return if a.sign { -1 } else { 1 };
        }
        // Same sign: compare by adjusted exponent first so that operands with
        // wildly differing exponents (e.g. 1E-391 vs 1E-999999999) never need
        // to align their coefficients (which would materialise 10^n).
        let adj_a = a.exp + _decimal_core::coeff_digits_of(&a.coeff) as isize - 1;
        let adj_b = b.exp + _decimal_core::coeff_digits_of(&b.coeff) as isize - 1;
        if adj_a != adj_b {
            let a_bigger = adj_a > adj_b;
            let pos = !a.sign;
            return if pos == a_bigger { 1 } else { -1 };
        }
        // Adjusted exponents match: align and compare coefficients. The powers
        // involved are bounded by the digit-count difference, so never huge.
        let ne = a.exp.min(b.exp);
        let ca = &a.coeff * BigInt::from(10).pow((a.exp - ne) as u32);
        let cb = &b.coeff * BigInt::from(10).pow((b.exp - ne) as u32);
        use core::cmp::Ordering;
        match ca.cmp(&cb) {
            Ordering::Less => if a.sign { 1 } else { -1 },
            Ordering::Equal => 0,
            Ordering::Greater => if a.sign { -1 } else { 1 },
        }
    }

    /// CPython tie-break for `max`: when two operands are numerically equal,
    /// the chosen operand depends on representation (sign and exponent), not
    /// on argument order. Returns true when the first operand `a` is the one
    /// CPython selects. Same rule is reused by `max_mag` on a magnitude tie.
    /// (Restored from f38aaae14 — f0a813ee2 accidentally reverted it.)
    fn max_picks_a(a: &Dec, b: &Dec) -> bool {
        match dec_cmp(a, b) {
            1 => true,
            -1 => false,
            _ => {
                if a.sign != b.sign {
                    // +0 vs -0 (only equal values with differing sign): max
                    // prefers the positive one.
                    !a.sign
                } else if a.sign {
                    // both negative: smaller exponent wins.
                    a.exp <= b.exp
                } else {
                    // both positive: larger exponent wins.
                    a.exp >= b.exp
                }
            }
        }
    }

    /// CPython tie-break for `min` (mirror of `max`).
    fn min_picks_a(a: &Dec, b: &Dec) -> bool {
        match dec_cmp(a, b) {
            -1 => true,
            1 => false,
            _ => {
                if a.sign != b.sign {
                    a.sign
                } else if a.sign {
                    a.exp >= b.exp
                } else {
                    a.exp <= b.exp
                }
            }
        }
    }

    /// `max_mag` selects the larger-magnitude operand; on a magnitude tie it
    /// applies the same representation-based selection as `max`.
    fn max_mag_picks_a(a: &Dec, b: &Dec) -> bool {
        let xa = Dec { sign: false, coeff: a.coeff.clone(), exp: a.exp, special: a.special };
        let ya = Dec { sign: false, coeff: b.coeff.clone(), exp: b.exp, special: b.special };
        match dec_cmp(&xa, &ya) {
            1 => true,
            -1 => false,
            _ => max_picks_a(a, b),
        }
    }

    /// `min_mag` (mirror of `max_mag`).
    fn min_mag_picks_a(a: &Dec, b: &Dec) -> bool {
        let xa = Dec { sign: false, coeff: a.coeff.clone(), exp: a.exp, special: a.special };
        let ya = Dec { sign: false, coeff: b.coeff.clone(), exp: b.exp, special: b.special };
        match dec_cmp(&xa, &ya) {
            -1 => true,
            1 => false,
            _ => min_picks_a(a, b),
        }
    }

    /// Convert a Python object (Decimal/int/float/str) to Dec.
    /// Used by Decimal constructor — accepts str.
    fn to_dec(obj: &PyObject, vm: &VirtualMachine) -> PyResult<Dec> {
        if let Some(pd) = obj.downcast_ref::<PyDecimal>() {
            return Ok(pd.d.clone());
        }
        if let Some(i) = obj.downcast_ref::<PyInt>() {
            return Ok(_decimal_core::dec_from_int(vm, i));
        }
        if let Some(f) = obj.downcast_ref::<PyFloat>() {
            return Ok(_decimal_core::dec_from_float(f.to_f64()));
        }
        if let Some(s) = obj.downcast_ref::<PyStr>() {
            let txt = s.as_wtf8().to_string();
            return _decimal_core::parse_decimal(&txt)
                .map_err(|e| new_decimal_error(vm, "InvalidOperation", &e));
        }
        // Tuples/lists as (sign, digits, exponent) triples.
        if let Some(t) = obj.downcast_ref::<PyTuple>() {
            return PyDecimal::from_tuple(t.as_slice(), vm);
        }
        if let Some(lst) = obj.downcast_ref::<crate::vm::builtins::PyList>() {
            let items: Vec<PyObjectRef> = lst.borrow_vec().to_vec();
            return PyDecimal::from_tuple(&items, vm);
        }
        if obj.downcast_ref::<crate::vm::builtins::PyList>().is_some() {
            return Err(vm.new_value_error("conversion from list to Decimal is not supported".to_string()));
        }
        Err(vm.new_type_error(format!("cannot convert {} to Decimal", obj.class().name())))
    }

    /// Convert a Python object (Decimal/int/float) to Dec.
    /// Used by Context methods — rejects str (CPython behavior).
    fn to_dec_ctx(obj: &PyObject, vm: &VirtualMachine) -> PyResult<Dec> {
        if let Some(pd) = obj.downcast_ref::<PyDecimal>() {
            return Ok(pd.d.clone());
        }
        if let Some(i) = obj.downcast_ref::<PyInt>() {
            return Ok(_decimal_core::dec_from_int(vm, i));
        }
        // CPython: Context methods reject float and str (TypeError).
        Err(vm.new_type_error(format!("conversion from {} to Decimal is not supported", obj.class().name())))
    }

    /// Quiet a NaN (signaling or not), truncating the payload to the context's
    /// maximum length (prec - clamp), matching CPython's `_fix_nan`.
    fn quiet_nan(x: &Dec, ctx: &Ctx) -> Dec {
        let max_len = (ctx.prec - ctx.clamp).max(1) as usize;
        let digits = _decimal_core::coeff_digits_of(&x.coeff);
        let coeff = if digits > max_len {
            let s = x.coeff.to_string();
            let start = s.len().saturating_sub(max_len);
            let stripped = s[start..].trim_start_matches('0');
            if stripped.is_empty() {
                BigInt::from(0)
            } else {
                stripped.parse::<BigInt>().unwrap_or_else(|_| BigInt::from(0))
            }
        } else {
            x.coeff.clone()
        };
        Dec { sign: x.sign, coeff, exp: 0, special: Special::NaN }
    }

    /// CPython `Decimal._check_nans`: a signaling NaN operand signals
    /// InvalidOperation and returns a quiet NaN (self before other); a quiet
    /// NaN propagates (self before other). Returns Some(result) when a NaN was
    /// handled, else None.
    fn check_nans(
        vm: &VirtualMachine,
        ctx: &Ctx,
        x: &Dec,
        y: Option<&Dec>,
    ) -> PyResult<Option<Dec>> {
        let x_nan = x.special == Special::NaN || x.special == Special::SignalingNaN;
        let y_nan = y.map_or(false, |d| {
            d.special == Special::NaN || d.special == Special::SignalingNaN
        });
        if !x_nan && !y_nan {
            return Ok(None);
        }
        if x.special == Special::SignalingNaN {
            ctx.set_flag(vm, "InvalidOperation")?;
            if ctx.trap(vm, "InvalidOperation")? {
                return Err(new_decimal_error(vm, "InvalidOperation", "sNaN"));
            }
            return Ok(Some(quiet_nan(x, ctx)));
        }
        if let Some(d) = y {
            if d.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "sNaN"));
                }
                return Ok(Some(quiet_nan(d, ctx)));
            }
        }
        if x_nan {
            return Ok(Some(quiet_nan(x, ctx)));
        }
        Ok(Some(quiet_nan(y.unwrap(), ctx)))
    }

    /// Next representable value from `x` in direction `dir` (+1 up, -1 down),
    /// matching CPython `next_plus`/`next_minus`. The caller must already have
    /// handled NaN operands. Computation runs on a scratch context where all
    /// flags and traps are ignored (CPython context.copy()._ignore_all_flags),
    /// so no flags are raised on the real context here.
    fn next_value(vm: &VirtualMachine, ctx: &Ctx, x: &Dec, dir: i8) -> PyResult<Dec> {
        // +/-Infinity: moving in the same direction returns self; the opposite
        // direction returns the largest finite value (prec nines at Etop),
        // preserving self's sign.
        if x.special == Special::Infinity {
            if (dir > 0 && !x.sign) || (dir < 0 && x.sign) {
                return Ok(x.clone());
            }
            let etop = ctx.emax - (ctx.prec - 1);
            let nines = "9".repeat(ctx.prec.max(1) as usize);
            return Ok(Dec {
                sign: x.sign,
                coeff: nines.parse::<BigInt>().unwrap_or_else(|_| BigInt::from(9)),
                exp: etop,
                special: Special::Finite,
            });
        }
        let rctx = Ctx::from_parts(
            ctx.prec,
            if dir < 0 { Rounding::Floor } else { Rounding::Ceiling },
            ctx.emax,
            ctx.emin,
            ctx.clamp,
            vm.ctx.new_dict(),
            vm.ctx.new_dict(),
        );
        // Zero: _fix leaves it zero, so move straight to the tiny increment
        // (0 +/- 1E(Etiny-1)), avoiding a pointless 10^(exp-e) alignment.
        if x.is_zero() {
            let etiny = ctx.emin - (ctx.prec - 1);
            let shifted = Dec {
                sign: dir < 0,
                coeff: BigInt::from(1),
                exp: etiny - 1,
                special: Special::Finite,
            };
            return _decimal_core::apply_context(vm, &rctx, shifted);
        }
        // _fix(self): rounding to the precision may itself yield the next
        // representable value (operands beyond precision, e.g. 1.00000010998).
        let new_self = _decimal_core::apply_context(vm, &rctx, x.clone())?;
        if dec_cmp(&new_self, x) != 0 {
            return Ok(new_self);
        }
        // Self is on the lattice: move by 10^(Etiny-1) and re-round.
        let etiny = ctx.emin - (ctx.prec - 1);
        let tiny_exp = etiny - 1;
        let e = tiny_exp.min(x.exp);
        let d_shift = BigInt::from(10).pow((x.exp - e) as u32);
        let inc_shift = BigInt::from(10).pow((tiny_exp - e) as u32);
        let signed_x = if x.sign { -(&x.coeff * &d_shift) } else { &x.coeff * &d_shift };
        let inc = BigInt::from(1) * &inc_shift;
        let new_signed: BigInt = if dir < 0 { signed_x - inc } else { signed_x + inc };
        let (coeff, sign) = if new_signed < BigInt::from(0) {
            (-new_signed.clone(), true)
        } else {
            (new_signed.clone(), false)
        };
        let shifted = Dec { sign, coeff, exp: e, special: Special::Finite };
        _decimal_core::apply_context(vm, &rctx, shifted)
    }

    /// next_plus: smallest representable larger than self. Raises only
    /// InvalidOperation for a signaling NaN (CPython ignores all other flags).
    fn next_plus_core(vm: &VirtualMachine, ctx: &Ctx, x: &Dec) -> PyResult<Dec> {
        if let Some(nan) = check_nans(vm, ctx, x, None)? {
            return Ok(nan);
        }
        next_value(vm, ctx, x, 1)
    }

    /// next_minus: largest representable smaller than self.
    fn next_minus_core(vm: &VirtualMachine, ctx: &Ctx, x: &Dec) -> PyResult<Dec> {
        if let Some(nan) = check_nans(vm, ctx, x, None)? {
            return Ok(nan);
        }
        next_value(vm, ctx, x, -1)
    }

    /// next_toward: closest number to self in the direction of y. Equal
    /// operands return self with y's sign; otherwise next_plus/next_minus is
    /// used and the CPython-mandated flags are raised on the real context
    /// (Overflow/Inexact/Rounded for an infinite result; Underflow/Subnormal/
    /// Inexact/Rounded, plus Clamped when the result is zero, for a subnormal).
    fn next_toward_core(vm: &VirtualMachine, ctx: &Ctx, x: &Dec, y: &Dec) -> PyResult<Dec> {
        if let Some(nan) = check_nans(vm, ctx, x, Some(y))? {
            return Ok(nan);
        }
        if dec_cmp(x, y) == 0 {
            // copy_sign(self, other): self's coefficient/exponent, other's sign.
            return Ok(Dec { sign: y.sign, coeff: x.coeff.clone(), exp: x.exp, special: x.special });
        }
        let dir = if dec_cmp(y, x) < 0 { -1i8 } else { 1i8 };
        let ans = next_value(vm, ctx, x, dir)?;
        if ans.special == Special::Infinity {
            ctx.set_flag(vm, "Overflow")?;
            if ctx.trap(vm, "Overflow")? {
                return Err(new_decimal_error(vm, "Overflow", "Infinite result from next_toward"));
            }
            ctx.set_flag(vm, "Inexact")?;
            if ctx.trap(vm, "Inexact")? {
                return Err(new_decimal_error(vm, "Inexact", "inexact result"));
            }
            ctx.set_flag(vm, "Rounded")?;
            if ctx.trap(vm, "Rounded")? {
                return Err(new_decimal_error(vm, "Rounded", "rounded result"));
            }
        } else if !ans.is_special() {
            let adj = ans.exp + _decimal_core::coeff_digits_of(&ans.coeff) as isize - 1;
            if adj < ctx.emin {
                ctx.set_flag(vm, "Underflow")?;
                if ctx.trap(vm, "Underflow")? {
                    return Err(new_decimal_error(vm, "Underflow", "underflow"));
                }
                ctx.set_flag(vm, "Subnormal")?;
                if ctx.trap(vm, "Subnormal")? {
                    return Err(new_decimal_error(vm, "Subnormal", "subnormal"));
                }
                ctx.set_flag(vm, "Inexact")?;
                if ctx.trap(vm, "Inexact")? {
                    return Err(new_decimal_error(vm, "Inexact", "inexact result"));
                }
                ctx.set_flag(vm, "Rounded")?;
                if ctx.trap(vm, "Rounded")? {
                    return Err(new_decimal_error(vm, "Rounded", "rounded result"));
                }
                if ans.is_zero() {
                    ctx.set_flag(vm, "Clamped")?;
                    if ctx.trap(vm, "Clamped")? {
                        return Err(new_decimal_error(vm, "Clamped", "clamped"));
                    }
                }
            }
        }
        Ok(ans)
    }

    /// Resolve the context for a Decimal method call: an explicit
    /// `context=` keyword/arg overrides the current context (CPython).
    fn ctx_from_opt(
        context: OptionalArg<PyObjectRef>,
        vm: &VirtualMachine,
    ) -> PyResult<Ctx> {
        if let Some(c) = context.into_option() {
            if c.is(&vm.ctx.none()) {
                return current_ctx(vm);
            }
            let pc = c
                .downcast_ref::<PyContext>()
                .ok_or_else(|| vm.new_type_error("context must be a Context object".to_string()))?;
            Ok(pc.ctx())
        } else {
            current_ctx(vm)
        }
    }

    /// True when the object is the None singleton (used to skip None-valued
    /// Context constructor fields).
    fn downcast_ref_none(v: &PyObject, vm: &VirtualMachine) -> bool {
        v.is(&vm.ctx.none())
    }

    /// Resolve the context from FuncArgs (accepts a `context=` keyword or a
    /// positional Context argument).
    fn ctx_from_funcargs(args: &FuncArgs, vm: &VirtualMachine) -> PyResult<Ctx> {
        if let Some(c) = args.get_optional_kwarg("context") {
            // None means "use the current context" (CPython).
            if c.is(&vm.ctx.none()) {
                return current_ctx(vm);
            }
            let pc = c
                .downcast_ref::<PyContext>()
                .ok_or_else(|| vm.new_type_error("context must be a Context object".to_string()))?;
            Ok(pc.ctx())
        } else {
            current_ctx(vm)
        }
    }

    /// Resolve the context for a no-arg method: rejects any positional args
    /// (TypeError, CPython parity) and accepts only context= keyword.
    fn ctx_from_noargs(args: &FuncArgs, vm: &VirtualMachine) -> PyResult<Ctx> {
        if !args.args.is_empty() {
            return Err(vm.new_type_error(
                "unexpected positional argument".to_string(),
            ));
        }
        ctx_from_funcargs(args, vm)
    }

    /// The 9 signals that can appear in a Context's flags/traps dict
    /// (CPython SignalDict keys). DecimalException and its non-signal
    /// subclasses are not signals.
    fn signal_types() -> Vec<&'static PyTypeRef> {
        _decimal_core::exc()
            .all()
            .into_iter()
            .filter(|ty| _decimal_core::is_signal(ty))
            .collect()
    }

    /// Extract the underlying plain dict from a value being assigned to a
    /// Context's flags/traps: either a plain dict or a live SignalDict.
    /// Anything else raises TypeError (cpython: "flags/traps must be a
    /// signal dict").
    fn signal_dict_value(
        value: PyObjectRef,
        name: &str,
        vm: &VirtualMachine,
    ) -> PyResult<PyDictRef> {
        if let Some(sd) = value.downcast_ref::<SignalDict>() {
            return sd.live_dict(vm);
        }
        if value.class().fast_issubclass(vm.ctx.types.dict_type) {
            return value
                .downcast::<PyDict>()
                .map_err(|_| vm.new_type_error(format!("{name} must be a signal dict")));
        }
        Err(vm.new_type_error(format!("{name} must be a signal dict")))
    }

    /// Validate a list/tuple of signals (used for Context(flags=...) and
    /// traps=... list/tuple forms). Elements may be signal-name strings or
    /// signal exception types. Non-string non-signal elements raise
    /// TypeError; unknown names raise KeyError (CPython).
    fn validate_signal_names(items: &[PyObjectRef], vm: &VirtualMachine) -> PyResult<()> {
        for item in items {
            if let Some(s) = item.downcast_ref::<PyStr>() {
                if _decimal_core::exc().by_name(&s.as_wtf8().to_string()).is_none() {
                    let name = s.as_wtf8().to_string();
                    return Err(vm.new_key_error(vm.ctx.new_str(name).into()));
                }
            } else if !signal_types().iter().any(|ty| item.is(ty.as_object())) {
                return Err(vm.new_type_error(
                    "flags/traps must contain signal names or signal classes".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// Digit-wise logical binary operation shared by the Decimal methods and
    /// the Context methods. CPython's `_islogical` requires a non-negative
    /// operand with exponent 0 whose coefficient digits are all 0 or 1; on a
    /// violation it signals InvalidOperation and returns NaN, raising only
    /// when the context traps it.
    fn logical_binary_op<F>(
        vm: &VirtualMachine,
        ctx: &Ctx,
        a: &Dec,
        b: &Dec,
        name: &str,
        op: F,
    ) -> PyResult<Dec>
    where
        F: Fn(u32, u32) -> u32,
    {
        let nan = Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN };
        // Specials are never logical: NaN and Infinity carry exponent 0 and a
        // zero sign, so the exp/sign checks alone would let them through
        // (andx834: and 0 NaN -> NaN; CPython rejects rather than computing 0).
        if a.special != Special::Finite
            || b.special != Special::Finite
            || a.exp != 0
            || b.exp != 0
            || a.sign
            || b.sign
        {
            ctx.set_flag(vm, "InvalidOperation")?;
            if ctx.trap(vm, "InvalidOperation")? {
                return Err(new_decimal_error(vm, "InvalidOperation", name));
            }
            return Ok(nan);
        }
        if !logical_valid(vm, ctx, &a.coeff)? || !logical_valid(vm, ctx, &b.coeff)? {
            return Ok(nan);
        }
        Ok(Dec {
            sign: false,
            coeff: logical_digit_op(&a.coeff, &b.coeff, ctx.prec, op),
            exp: 0,
            special: Special::Finite,
        })
    }

/// raises InvalidOperation (with flag) otherwise, matching CPython.
    /// Drop the fractional part of a decimal, truncating toward zero and
    /// preserving the sign of zero (divmod(1, -2) -> (-0, 1)).
    fn truncate_toward_zero(d: &Dec) -> Dec {
        if d.is_special() || d.is_zero() || d.exp >= 0 {
            return d.clone();
        }
        let divisor = BigInt::from(10).pow((-d.exp) as u32);
        Dec {
            sign: d.sign,
            coeff: &d.coeff / &divisor,
            exp: 0,
            special: Special::Finite,
        }
    }

fn logical_valid(vm: &VirtualMachine, ctx: &Ctx, coeff: &BigInt) -> PyResult<bool> {
    let s = coeff.to_string();
    if s.chars().all(|c| c == '0' || c == '1') {
        return Ok(true);
    }
    ctx.set_flag(vm, "InvalidOperation")?;
    if ctx.trap(vm, "InvalidOperation")? {
        return Err(new_decimal_error(vm, "InvalidOperation", "logical operation requires 0/1 digits"));
    }
    Ok(false)
}

/// Digit-wise logical operation on decimal coefficients. The result keeps
/// the LOW `prec` digits (CPython: result length = context precision).
fn logical_digit_op<F>(a: &BigInt, b: &BigInt, prec: isize, f: F) -> BigInt
    where
        F: Fn(u32, u32) -> u32,
    {
        let mut sa = a.to_string();
        let mut sb = b.to_string();
        let width = sa.len().max(sb.len()).max(1);
        while sa.len() < width {
            sa.insert(0, '0');
        }
        while sb.len() < width {
            sb.insert(0, '0');
        }
        let mut out = String::new();
        for (ca, cb) in sa.chars().zip(sb.chars()) {
            let da = ca.to_digit(10).unwrap_or(0);
            let db = cb.to_digit(10).unwrap_or(0);
            out.push_str(&f(da, db).to_string());
        }
        // Keep only the low `prec` digits (rightmost), trim leading zeros.
        let keep = prec.max(0) as usize;
        if out.len() > keep && keep > 0 {
            out = out[out.len() - keep..].to_string();
        }
        out.trim_start_matches('0');
        out.parse::<BigInt>().unwrap_or_else(|_| BigInt::from(0))
    }

    #[pyattr(name = "Decimal")]
    #[pyclass(name = "Decimal", module = "decimal", traverse)]
    #[derive(Debug, PyPayload)]
    struct PyDecimal {
        #[pytraverse(skip)]
        d: Dec,
    }

    impl PyDecimal {
        fn to_py(vm: &VirtualMachine, d: Dec) -> PyObjectRef {
            PyDecimal { d }.into_ref(&vm.ctx).into()
        }

        fn from_tuple(items: &[PyObjectRef], vm: &VirtualMachine) -> PyResult<Dec> {
            if items.len() != 3 {
                return Err(vm.new_value_error("tuple must be (sign, digits, exponent)".to_string()));
            }
            // CPython raises ValueError when the sign/exponent isn't an int
            // (e.g. a float 0.0).
            let sign_val: isize = match items[0].downcast_ref::<PyInt>() {
                Some(i) => i.try_to_primitive::<isize>(vm)?,
                None => {
                    return Err(vm.new_value_error(
                        "sign must be an integer".to_string(),
                    ))
                }
            };
            if sign_val != 0 && sign_val != 1 {
                return Err(vm.new_value_error("sign must be 0 or 1".to_string()));
            }
            let sign = sign_val == 1;
            let digit_items: Vec<PyObjectRef> = if let Some(t) = items[1].downcast_ref::<PyTuple>() {
                t.as_slice().to_vec()
            } else if let Some(lst) = items[1].downcast_ref::<crate::vm::builtins::PyList>() {
                lst.borrow_vec().to_vec()
            } else {
                // CPython: non-tuple digits -> ValueError.
                return Err(vm.new_value_error("coefficient must be a tuple".to_string()));
            };
            let mut coeff = BigInt::from(0);
            for d in digit_items {
                // Each digit must be an int in [0, 9]; anything else is a
                // ValueError (CPython).
                let dv: i64 = match d.downcast_ref::<PyInt>() {
                    Some(i) => i.try_to_primitive::<i64>(vm)?,
                    None => {
                        return Err(vm.new_value_error("coefficient digit must be an integer".to_string()))
                    }
                };
                if dv < 0 || dv > 9 {
                    return Err(vm.new_value_error(
                        "coefficient digit must be in [0, 9]".to_string(),
                    ));
                }
                coeff = &coeff * 10 + BigInt::from(dv);
            }
            // Exponent may be an int (finite) or a string naming a special
            // value: 'F'/'f' (Infinity), 'n'/'N' (NaN), 's'/'S' (sNaN).
            if let Some(s) = items[2].downcast_ref::<PyStr>() {
                let tag = s.as_wtf8().to_string();
                let special = match tag.as_str() {
                    "F" | "f" | "i" | "I" => Special::Infinity,
                    "n" => Special::NaN,
                    "N" | "s" | "S" => Special::SignalingNaN,
                    _ => {
                        // CPython raises ValueError for any other string
                        // (digits, 'wrong!' etc.) in the exponent slot.
                        return Err(vm.new_value_error(
                            "malformed exponent: must be an int or a special-value string".to_string(),
                        ))
                    }
                };
                return Ok(Dec { sign, coeff, exp: 0, special });
            }
            let exp: isize = match items[2].downcast_ref::<PyInt>() {
                Some(i) => i.try_to_primitive::<isize>(vm)?,
                None => {
                    return Err(vm.new_value_error(
                        "exponent must be an integer".to_string(),
                    ))
                }
            };
            Ok(Dec { sign, coeff, exp, special: Special::Finite })
        }
    }

    #[pyclass(with(Constructor, AsNumber, Comparable, Hashable, Representable))]
    impl PyDecimal {
        #[pymethod]
        fn quantize(zelf: &Py<Self>, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            // Support positional or keyword: quantize(exp, rounding=, context=)
            let get = |key: &str| -> Option<PyObjectRef> {
                args.get_optional_kwarg(key).or_else(|| None)
            };
            let exp = get("exp").or_else(|| args.args.get(0).cloned()).ok_or_else(|| {
                vm.new_type_error("quantize requires at least one argument".to_string())
            })?;
            let target_exp: isize = if let Some(pd) = exp.downcast_ref::<PyDecimal>() {
                pd.d.exp
            } else if let Some(_i) = exp.downcast_ref::<PyInt>() {
                // CPython converts an int to Decimal(int), whose exponent is 0.
                0
            } else if let Some(s) = exp.downcast_ref::<PyStr>() {
                _decimal_core::parse_decimal(&s.as_wtf8().to_string())
                    .map_err(|e| new_decimal_error(vm, "InvalidOperation", &e))?
                    .exp
            } else {
                return Err(vm.new_type_error(
                    "quantize requires a Decimal, int or str exponent".to_string(),
                ));
            };
            // The rounding arg may be positional (2nd) or a keyword; None
            // means "use the context rounding"; a non-string is TypeError.
            // Passing both is a TypeError (multiple values for 'rounding').
            if args.get_optional_kwarg("rounding").is_some() && args.args.len() >= 2 {
                return Err(vm.new_type_error(
                    "quantize() got multiple values for argument 'rounding'".to_string(),
                ));
            }
            let rounding_arg = get("rounding").or_else(|| args.args.get(1).cloned());
            if args.args.len() > 2 {
                return Err(vm.new_type_error(
                    "quantize takes at most 3 positional arguments".to_string(),
                ));
            }
            let rounding_str: Option<String> = match rounding_arg {
                Some(v) if v.is(&vm.ctx.none()) => None,
                Some(v) => match v.downcast_ref::<PyStr>() {
                    Some(s) => Some(s.as_wtf8().to_string()),
                    None => {
                        return Err(vm.new_type_error(
                            "rounding must be a string".to_string(),
                        ))
                    }
                },
                None => None,
            };
            // context= keyword overrides the current context; None means "use the
            // current context".
            let mut ctx = if let Some(c) = get("context") {
                if c.is(&vm.ctx.none()) {
                    current_ctx(vm)?
                } else {
                    let pc = c.downcast_ref::<PyContext>().ok_or_else(|| {
                        vm.new_type_error("context must be a Context object".to_string())
                    })?;
                    pc.ctx()
                }
            } else {
                current_ctx(vm)?
            };
            if let Some(ref rs) = rounding_str {
                ctx.rounding = Rounding::from_str(rs)
                    .ok_or_else(|| vm.new_value_error("invalid rounding mode".to_string()))?;
            }
            let r = _decimal_core::quantize(vm, &ctx, &zelf.d, target_exp)?;
            Ok(Self::to_py(vm, r))
        }

        #[pymethod]
        fn as_tuple(&self, vm: &VirtualMachine) -> PyResult {
            let sign = if self.d.sign { 1 } else { 0 };
            // CPython as_tuple returns a DecimalTuple (namedtuple):
            // finite -> (sign, digits, int exp); special -> string exp
            // 'F' (Infinity), 'n' (NaN), 'N' (sNaN); NaN keeps its
            // diagnostic digits.
            let diag_digits = || -> Vec<PyObjectRef> {
                if self.d.coeff == BigInt::from(0) {
                    return vec![];
                }
                self.d.coeff_str()
                    .bytes()
                    .map(|b| vm.ctx.new_int(b - b'0').into())
                    .collect()
            };
            let (digits_tuple, exp_obj): (PyObjectRef, PyObjectRef) = match self.d.special {
                Special::Finite => {
                    let digits: Vec<PyObjectRef> = self
                        .d
                        .coeff_str()
                        .bytes()
                        .map(|b| vm.ctx.new_int(b - b'0').into())
                        .collect();
                    (vm.ctx.new_tuple(digits).into(), vm.ctx.new_int(self.d.exp).into())
                }
                Special::Infinity => (
                    vm.ctx.new_tuple(vec![vm.ctx.new_int(0).into()]).into(),
                    vm.ctx.new_str("F").into(),
                ),
                Special::NaN => (
                    vm.ctx.new_tuple(diag_digits()).into(),
                    vm.ctx.new_str("n").into(),
                ),
                Special::SignalingNaN => (
                    vm.ctx.new_tuple(diag_digits()).into(),
                    vm.ctx.new_str("N").into(),
                ),
            };
            // Wrap in the DecimalTuple namedtuple (sign, digits, exponent).
            // A process-level shared class so all _decimal module instances
            // (fresh re-imports) use the same type — matches the pickle
            // interchange test's identity expectations.
            let dt_cls = DECIMAL_TUPLE
                .lock()
                .clone()
                .unwrap_or_else(|| vm.ctx.types.tuple_type.to_owned().into());
            let sign_obj: PyObjectRef = vm.ctx.new_int(sign).into();
            let dt = vm.invoke(&dt_cls, (sign_obj, digits_tuple, exp_obj))?;
            Ok(dt)
        }

        #[pymethod]
        fn is_zero(&self) -> bool {
            self.d.is_zero()
        }
        #[pymethod]
        fn is_nan(&self) -> bool {
            self.d.special == Special::NaN || self.d.special == Special::SignalingNaN
        }
        #[pymethod]
        fn is_qnan(&self) -> bool {
            self.d.special == Special::NaN
        }
        #[pymethod]
        fn is_snan(&self) -> bool {
            self.d.special == Special::SignalingNaN
        }
        #[pymethod]
        fn is_infinite(&self) -> bool {
            self.d.special == Special::Infinity
        }
        #[pymethod]
        fn is_finite(&self) -> bool {
            self.d.special == Special::Finite
        }
        #[pymethod]
        fn is_signed(&self) -> bool {
            self.d.sign
        }
        #[pymethod]
        fn is_canonical(&self) -> bool {
            true
        }
        #[pymethod]
        fn is_normal(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult<bool> {
            let ctx = ctx_from_noargs(&args, vm)?;
            let etiny = ctx.emin - (ctx.prec - 1);
            Ok(!self.d.is_zero()
                && self.d.special == Special::Finite
                && self.d.exp >= etiny)
        }
        #[pymethod]
        fn is_subnormal(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult<bool> {
            let ctx = ctx_from_noargs(&args, vm)?;
            let etiny = ctx.emin - (ctx.prec - 1);
            Ok(!self.d.is_zero() && self.d.special == Special::Finite && self.d.exp < etiny)
        }

        #[pymethod]
        fn copy_abs(zelf: PyRef<Self>) -> PyDecimal {
            let mut d = zelf.d.clone();
            d.sign = false;
            PyDecimal { d }
        }
        #[pymethod]
        fn copy_negate(zelf: PyRef<Self>) -> PyDecimal {
            let mut d = zelf.d.clone();
            d.sign = !d.sign;
            PyDecimal { d }
        }

        // Unlike copy_abs/copy_negate, the unary operators round the result to
        // the current context's precision (and re-check its Emin/Emax), so they
        // must not simply fall back to the copy_* variants. The number slots
        // below implement exactly this.
        #[pymethod]
        fn copy_sign(zelf: PyRef<Self>, other: PyObjectRef, vm: &VirtualMachine) -> PyResult<PyDecimal> {
            let b = to_dec_ctx(&other, vm)?;
            let mut d = zelf.d.clone();
            d.sign = b.sign;
            Ok(PyDecimal { d })
        }

        #[pymethod]
        fn adjusted(&self) -> isize {
            let digits = self.d.coeff_digits() as isize;
            self.d.exp + digits - 1
        }

        #[pymethod]
        fn radix(&self, vm: &VirtualMachine) -> PyResult {
            Ok(vm.ctx.new_int(BigInt::from(10)).into())
        }

        #[pymethod]
        fn number_class(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult<String> {
            let ctx = ctx_from_noargs(&args, vm)?;
            match self.d.special {
                // NaN/sNaN classes carry no sign (class(+NaN12345) -> 'NaN').
                Special::NaN => return Ok("NaN".to_string()),
                Special::SignalingNaN => return Ok("sNaN".to_string()),
                Special::Infinity => return Ok(format!("{}Infinity", if self.d.sign { "-" } else { "+" })),
                Special::Finite => {}
            }
            let sign = if self.d.sign { "-" } else { "+" };
            if self.d.is_zero() {
                return Ok(format!("{sign}Zero"));
            }
            // Subnormal/Infinite are judged on the *adjusted* exponent
            // (exp + digits - 1): 0.99999999E-999 (coeff 99999999, exp -1007,
            // adjusted -1000) is Subnormal at Emin=-999 (clasx006).
            let etiny = ctx.emin - (ctx.prec - 1);
            let adj = self.d.exp + self.d.coeff_digits() as isize - 1;
            if adj < ctx.emin {
                Ok(format!("{sign}Subnormal"))
            } else if adj > ctx.emax {
                Ok(format!("{sign}Infinite"))
            } else {
                Ok(format!("{sign}Normal"))
            }
        }

        #[pymethod]
        fn normalize(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let ctx = ctx_from_noargs(&args, vm)?;
            // sNaN signals InvalidOperation and becomes a quiet NaN with the
            // payload truncated to the context precision (mirrors Context).
            if self.d.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "normalize with sNaN"));
                }
                let mut n = self.d.clone();
                n.special = Special::NaN;
                let keep = (ctx.prec as usize).max(1);
                let digits = n.coeff_str();
                if digits.len() > keep {
                    n.coeff = digits[digits.len() - keep..]
                        .parse::<BigInt>()
                        .unwrap_or_else(|_| BigInt::from(0));
                }
                return Ok(PyDecimal::to_py(vm, n));
            }
            let mut d = self.d.clone();
            if d.is_zero() {
                d.exp = 0;
                let r = _decimal_core::apply_context(vm, &ctx, d)?;
                return Ok(PyDecimal::to_py(vm, r));
            }
            while d.coeff_digits() > 1 && (&d.coeff % BigInt::from(10)) == BigInt::from(0) {
                d.coeff /= BigInt::from(10);
                d.exp += 1;
            }
            let mut r = _decimal_core::apply_context(vm, &ctx, d)?;
            // Underflow to zero: CPython normalizes to plain '0' (exp 0).
            if r.is_zero() {
                r.exp = 0;
            }
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn same_quantum(&self, other: PyObjectRef, vm: &VirtualMachine) -> PyResult<bool> {
            let b = to_dec(&other, vm)?;
            // decTest samq0211/0212/0312/0325: two NaNs are the same quantum,
            // two infinities are the same quantum, anything mixed with a
            // finite (or NaN vs Inf) is not (samq0317 NaN -0 -> 0). Finite
            // operands compare exponents.
            use Special::{Finite, Infinity, NaN, SignalingNaN};
            Ok(match (self.d.special, b.special) {
                (NaN | SignalingNaN, NaN | SignalingNaN) => true,
                (Infinity, Infinity) => true,
                (Finite, Finite) => self.d.exp == b.exp,
                _ => false,
            })
        }

        #[pymethod]
        fn compare(
            &self,
            args: FuncArgs,
            vm: &VirtualMachine,
        ) -> PyResult {
            let other = args
                .get_optional_kwarg("other")
                .or_else(|| args.args.get(0).cloned())
                .ok_or_else(|| vm.new_type_error("compare requires an argument".to_string()))?;
            let b = to_dec_ctx(&other, vm)?;
            let ctx = ctx_from_funcargs(&args, vm)?; // accept context arg
            // CPython: if either operand is a quiet NaN, the result is the
            // NaN (payload preserved); a signaling NaN raises InvalidOperation.
            if self.d.special == Special::SignalingNaN || b.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "compare with sNaN"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            if self.d.special == Special::NaN {
                let mut n = self.d.clone();
                // payload truncated to context precision
                let digits = n.coeff_str();
                let keep = ctx.prec as usize;
                if digits.len() > keep {
                    let keep = keep.max(1);
                    n.coeff = digits[digits.len() - keep..].parse::<BigInt>().unwrap_or_else(|_| BigInt::from(0));
                }
                return Ok(PyDecimal::to_py(vm, n));
            }
            if b.special == Special::NaN {
                let mut n = b;
                let digits = n.coeff_str();
                let keep = ctx.prec as usize;
                if digits.len() > keep {
                    let keep = keep.max(1);
                    n.coeff = digits[digits.len() - keep..].parse::<BigInt>().unwrap_or_else(|_| BigInt::from(0));
                }
                return Ok(PyDecimal::to_py(vm, n));
            }
            Ok(vm.ctx.new_int(BigInt::from(dec_cmp(&self.d, &b))).into())
        }

        #[pymethod]
        fn next_minus(
            &self,
            args: FuncArgs,
            vm: &VirtualMachine,
        ) -> PyResult {
            let ctx = ctx_from_noargs(&args, vm)?;
            Ok(PyDecimal::to_py(vm, self.next_step(vm, &ctx, -1)?))
        }

        #[pymethod]
        fn next_plus(
            &self,
            args: FuncArgs,
            vm: &VirtualMachine,
        ) -> PyResult {
            let ctx = ctx_from_noargs(&args, vm)?;
            Ok(PyDecimal::to_py(vm, self.next_step(vm, &ctx, 1)?))
        }

        fn next_step(
            &self,
            vm: &VirtualMachine,
            ctx: &Ctx,
            dir: i8,
        ) -> PyResult<Dec> {
            // next_plus/next_minus: a signaling NaN signals InvalidOperation and
            // quiets; otherwise compute the next representable value (the real
            // flag raising for next_toward is done by next_toward_core).
            if let Some(nan) = check_nans(vm, ctx, &self.d, None)? {
                return Ok(nan);
            }
            next_value(vm, ctx, &self.d, dir)
        }

        #[pymethod]
        fn next_toward(
            &self,
            args: FuncArgs,
            vm: &VirtualMachine,
        ) -> PyResult {
            let other = args
                .args
                .get(0)
                .cloned()
                .ok_or_else(|| vm.new_type_error("next_toward requires an argument".to_string()))?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let b = to_dec(&other, vm)?;
            Ok(PyDecimal::to_py(vm, next_toward_core(vm, &ctx, &self.d, &b)?))
        }

        #[pymethod]
        fn max(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let other = args
                .args
                .get(0)
                .cloned()
                .ok_or_else(|| vm.new_type_error("max requires an argument".to_string()))?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let b = to_dec_ctx(&other, vm)?;
            // sNaN raises InvalidOperation; quiet NaN is ignored (returns the
            // other operand, CPython).
            if self.d.special == Special::SignalingNaN || b.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "max with sNaN"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let chosen = if max_picks_a(&self.d, &b) { self.d.clone() } else { b };
            let r = _decimal_core::apply_context(vm, &ctx, chosen)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn min(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let other = args
                .args
                .get(0)
                .cloned()
                .ok_or_else(|| vm.new_type_error("min requires an argument".to_string()))?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let b = to_dec_ctx(&other, vm)?;
            if self.d.special == Special::SignalingNaN || b.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "min with sNaN"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let chosen = if min_picks_a(&self.d, &b) { self.d.clone() } else { b };
            let r = _decimal_core::apply_context(vm, &ctx, chosen)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn max_mag(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let other = args
                .args
                .get(0)
                .cloned()
                .ok_or_else(|| vm.new_type_error("max_mag requires an argument".to_string()))?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let b = to_dec_ctx(&other, vm)?;
            if self.d.special == Special::SignalingNaN || b.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "max_mag with sNaN"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let chosen = if max_mag_picks_a(&self.d, &b) { self.d.clone() } else { b };
            let r = _decimal_core::apply_context(vm, &ctx, chosen)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn min_mag(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let other = args
                .args
                .get(0)
                .cloned()
                .ok_or_else(|| vm.new_type_error("min_mag requires an argument".to_string()))?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let b = to_dec_ctx(&other, vm)?;
            if self.d.special == Special::SignalingNaN || b.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "min_mag with sNaN"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let chosen = if min_mag_picks_a(&self.d, &b) { self.d.clone() } else { b };
            let r = _decimal_core::apply_context(vm, &ctx, chosen)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn sqrt(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let ctx = ctx_from_noargs(&args, vm)?;
            let r = _decimal_core::dec_sqrt(vm, &ctx, &self.d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn exp(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
let ctx = ctx_from_noargs(&args, vm)?;
if self.d.is_special() {
match self.d.special {
// exp(+Infinity) = +Infinity; exp(-Infinity) = 0.
Special::Infinity => {
if self.d.sign {
return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Finite,
}));
}
let r = _decimal_core::apply_context(vm, &ctx, self.d.clone())?;
return Ok(PyDecimal::to_py(vm, r));
}
// Only a signaling NaN traps; a quiet NaN propagates.
Special::SignalingNaN => {
ctx.set_flag(vm, "InvalidOperation")?;
if ctx.trap(vm, "InvalidOperation")? {
return Err(new_decimal_error(vm, "InvalidOperation", "exp of sNaN"));
}
return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
sign: self.d.sign, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
}));
}
_ => return Ok(PyDecimal::to_py(vm, self.d.clone())),
}
}
// Overflow for a huge positive argument is decided inside dec_exp by
// comparing the result's decimal exponent against emax (exp(2.3e6) is still
// representable; only exp(>~2.3e6) overflows). A coarse threshold here
// would wrongly overflow the representable range.
            let r = _decimal_core::dec_exp(vm, &ctx, &self.d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn ln(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let ctx = ctx_from_noargs(&args, vm)?;
            // CPython: ln(0) = -Infinity; ln(negative) -> InvalidOperation.
            if self.d.is_zero() {
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: true, coeff: BigInt::from(0), exp: 0, special: Special::Infinity,
                }));
            }
            if self.d.is_special() {
                // ln/log10 of a special returns the special itself.
                return Ok(PyDecimal::to_py(vm, self.d.clone()));
            }
            if self.d.sign {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "ln of non-positive"));
                }
                // InvalidOperation yields NaN, not the operand.
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let r = _decimal_core::dec_ln(vm, &ctx, &self.d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn log10(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let ctx = ctx_from_noargs(&args, vm)?;
            // CPython: log10(0) = -Infinity; log10(negative) -> InvalidOperation.
            if self.d.is_zero() {
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: true, coeff: BigInt::from(0), exp: 0, special: Special::Infinity,
                }));
            }
            if self.d.is_special() {
                // ln/log10 of a special returns the special itself.
                return Ok(PyDecimal::to_py(vm, self.d.clone()));
            }
            if self.d.sign {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "log10 of non-positive"));
                }
                // InvalidOperation yields NaN, not the operand.
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let r = _decimal_core::dec_log10(vm, &ctx, &self.d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn logb(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let ctx = ctx_from_noargs(&args, vm)?;
            if self.d.is_special() {
                if self.d.special == Special::Infinity {
                    return Ok(PyDecimal::to_py(vm, self.d.clone()));
                }
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "logb of NaN"));
                }
                return Ok(PyDecimal::to_py(vm, self.d.clone()));
            }
            if self.d.is_zero() {
                ctx.set_flag(vm, "DivisionByZero")?;
                if ctx.trap(vm, "DivisionByZero")? {
                    return Err(new_decimal_error(vm, "DivisionByZero", "logb of zero"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec { sign: self.d.sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity }));
            }
            let adj = self.d.exp + _decimal_core::coeff_digits_of(&self.d.coeff) as isize - 1;
            Ok(PyDecimal::to_py(vm, _decimal_core::Dec { sign: adj < 0, coeff: BigInt::from(adj.unsigned_abs()), exp: 0, special: Special::Finite }))
        }

        #[pymethod]
        fn compare_signal(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let other = args
                .args
                .get(0)
                .cloned()
                .ok_or_else(|| vm.new_type_error("compare_signal requires an argument".to_string()))?;
            let b = to_dec_ctx(&other, vm)?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let is_nan = |d: &Dec| {
                d.special == Special::NaN || d.special == Special::SignalingNaN
            };
            if is_nan(&self.d) || is_nan(&b) {
                // compare_signal: a NaN operand signals InvalidOperation and,
                // when not trapped, yields a quiet NaN with that operand's
                // sign/payload (compare_signal(-NaN, +NaN) -> -NaN).
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "comparison with NaN"));
                }
                let src = if is_nan(&self.d) { &self.d } else { &b };
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: src.sign,
                    coeff: src.coeff.clone(),
                    exp: 0,
                    special: Special::NaN,
                }));
            }
            let r = dec_cmp(&self.d, &b);
            Ok(vm.ctx.new_int(BigInt::from(r)).into())
        }

        #[pymethod]
        fn compare_total(&self, other: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let b = to_dec(&other, vm)?;
            let order = |d: &Dec| -> i64 {
                match d.special {
                    Special::SignalingNaN => if d.sign { 0 } else { 9 },
                    Special::NaN => if d.sign { 1 } else { 10 },
                    Special::Infinity => if d.sign { 2 } else { 8 },
                    Special::Finite => { if d.is_zero() { if d.sign { 3 } else { 7 } } else { if d.sign { 4 } else { 6 } } }
                }
            };
            let ox = order(&self.d);
            let oy = order(&b);
            let r = if ox != oy {
                match ox.cmp(&oy) { core::cmp::Ordering::Less => -1, core::cmp::Ordering::Equal => 0, core::cmp::Ordering::Greater => 1 }
            } else {
                let is_nan = |d: &Dec| {
                    d.special == Special::NaN || d.special == Special::SignalingNaN
                };
                if is_nan(&self.d) && is_nan(&b) {
                    nan_payload_cmp(&self.d, &b)
                } else {
                    dec_cmp(&self.d, &b)
                }
            };
            Ok(vm.ctx.new_int(BigInt::from(r)).into())
        }

        #[pymethod]
        fn compare_total_mag(&self, other: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let mut xd = self.d.clone(); xd.sign = false;
            let mut yd = to_dec(&other, vm)?; yd.sign = false;
            let order = |d: &Dec| -> i64 {
                match d.special {
                    Special::SignalingNaN => if d.sign { 0 } else { 9 },
                    Special::NaN => if d.sign { 1 } else { 10 },
                    Special::Infinity => if d.sign { 2 } else { 8 },
                    Special::Finite => { if d.is_zero() { if d.sign { 3 } else { 7 } } else { if d.sign { 4 } else { 6 } } }
                }
            };
            let ox = order(&xd);
            let oy = order(&yd);
            let r = if ox != oy {
                match ox.cmp(&oy) { core::cmp::Ordering::Less => -1, core::cmp::Ordering::Equal => 0, core::cmp::Ordering::Greater => 1 }
            } else {
                let is_nan = |d: &Dec| {
                    d.special == Special::NaN || d.special == Special::SignalingNaN
                };
                if is_nan(&xd) && is_nan(&yd) {
                    nan_payload_cmp(&xd, &yd)
                } else {
                    dec_cmp(&xd, &yd)
                }
            };
            Ok(vm.ctx.new_int(BigInt::from(r)).into())
        }

        #[pymethod]
        fn __str__(zelf: &Py<Self>, vm: &VirtualMachine) -> PyStrRef {
            vm.ctx.new_str(zelf.d.to_string())
        }

        #[pymethod]
        fn __sizeof__(&self, vm: &VirtualMachine) -> PyResult<usize> {
            // CPython: sizeof = base + 8 bytes per 19-digit limb of the
            // coefficient (0 coefficient has no limbs).
            let digits = _decimal_core::coeff_digits_of(&self.d.coeff) as usize;
            let limbs = if digits == 0 { 0 } else { (digits + 18) / 19 };
            Ok(104 + 8 * limbs)
        }

        #[pymethod]
        fn __format__(
            &self,
            spec: PyStrRef,
            localeconv: OptionalArg<PyObjectRef>,
            vm: &VirtualMachine,
        ) -> PyResult<String> {
            let spec = spec.as_wtf8().to_string();
            if spec.is_empty() {
                return Ok(self.d.to_string());
            }
            // Deprecated 'N' type: emits a DeprecationWarning and formats
            // like uppercase 'n' (CPython).
            if spec.ends_with('N') {
                let _ = crate::vm::stdlib::_warnings::warn(
                    vm.ctx.exceptions.deprecation_warning,
                    "The 'N' format code is deprecated in Python and will be removed in the future.".to_owned(),
                    1,
                    vm,
                );
            }
            // The second positional arg is localeconv data (dict) for the
            // 'n' format; it must be None or a dict (TypeError otherwise).
            let localeconv_opt = localeconv.into_option();
            if let Some(lc) = &localeconv_opt {
                if !lc.is(&vm.ctx.none()) && lc.downcast_ref::<PyDict>().is_none() {
                    return Err(vm.new_type_error(
                        "localeconv must be a dict or None".to_string(),
                    ));
                }
            }
            let ctx = current_ctx(vm)?;
            let capitals = current_capitals(vm)?;
            // Build Locale data from the optional localeconv dict.
            let mut locale = _decimal_core::Locale::default();
            if let Some(lc) = localeconv_opt
                && let Some(d) = lc.downcast_ref::<PyDict>()
            {
                let get_str = |key: &str| -> String {
                    d.get_item_opt(vm.ctx.intern_str(key), vm)
                        .ok()
                        .flatten()
                        .and_then(|v| v.downcast_ref::<PyStr>().map(|s| s.as_wtf8().to_string()))
                        .unwrap_or_default()
                };
                locale.decimal_point = get_str("decimal_point");
                locale.thousands_sep = get_str("thousands_sep");
                // grouping is a string of byte codes (e.g. "\x03\x03\0")
                if let Some(g) = d.get_item_opt(vm.ctx.intern_str("grouping"), vm)
                    .ok().flatten()
                    .and_then(|v| v.downcast_ref::<PyStr>().map(|s| s.as_bytes().to_vec()))
                {
                    locale.grouping = g.into_iter().map(|b| b as i32).collect();
                    // grouping byte codes must be <= CHAR_MAX (127);
                    // larger values (e.g. 255) are invalid overrides.
                    if locale.grouping.iter().any(|&v| v > 127) {
                        return Err(vm.new_value_error(
                            "invalid grouping override in localeconv".to_string(),
                        ));
                    }
                }
                // decimal_point / thousands_sep must be at most one character.
                if locale.decimal_point.chars().count() > 1
                    || locale.thousands_sep.chars().count() > 1
                {
                    return Err(vm.new_value_error(
                        "invalid locale separator override".to_string(),
                    ));
                }
            }
            _decimal_core::format_decimal(&self.d, &spec, vm, &ctx, capitals, &locale)
                .map_err(|e| vm.new_value_error(e))
        }

        // Reflected ops are handled by the AsNumber right_* slots
        // (copy_from sets right_add = add, and binop_num checks both args).

        #[pymethod]
        fn __trunc__(&self, vm: &VirtualMachine) -> PyResult {
            let d = &self.d;
            if d.is_special() {
                return match d.special {
                    Special::Infinity => Err(vm.new_overflow_error("cannot convert Infinity to integer".to_string())),
                    Special::NaN | Special::SignalingNaN => Err(vm.new_value_error("cannot convert NaN to integer".to_string())),
                    Special::Finite => unreachable!(),
                };
            }
            let mut coeff = d.coeff.clone();
            if d.exp < 0 {
                let pow = BigInt::from(10).pow((-d.exp) as u32);
                coeff = &coeff / &pow;
            } else if d.exp > 0 {
                coeff *= BigInt::from(10).pow(d.exp as u32);
            }
            let v = if self.d.sign { -coeff } else { coeff };
            Ok(vm.ctx.new_int(v).into())
        }

        #[pymethod]
        fn __floor__(&self, vm: &VirtualMachine) -> PyResult {
            let d = &self.d;
            if d.is_special() {
                return match d.special {
                    Special::Infinity => Err(vm.new_overflow_error("cannot convert Infinity to integer".to_string())),
                    Special::NaN | Special::SignalingNaN => Err(vm.new_value_error("cannot convert NaN to integer".to_string())),
                    Special::Finite => unreachable!(),
                };
            }
            // CPython: math.floor rounds toward -Inf.
            let mut coeff = d.coeff.clone();
            let mut had_remainder = false;
            if d.exp < 0 {
                let pow = BigInt::from(10).pow((-d.exp) as u32);
                let r = &coeff % &pow;
                had_remainder = r != BigInt::from(0);
                coeff = &coeff / &pow;
            } else if d.exp > 0 {
                coeff *= BigInt::from(10).pow(d.exp as u32);
            }
            let v = if d.sign {
                if had_remainder {
                    -(coeff + BigInt::from(1))
                } else {
                    -coeff
                }
            } else {
                coeff
            };
            Ok(vm.ctx.new_int(v).into())
        }

        #[pymethod]
        fn __ceil__(&self, vm: &VirtualMachine) -> PyResult {
            let d = &self.d;
            if d.is_special() {
                return match d.special {
                    Special::Infinity => Err(vm.new_overflow_error("cannot convert Infinity to integer".to_string())),
                    Special::NaN | Special::SignalingNaN => Err(vm.new_value_error("cannot convert NaN to integer".to_string())),
                    Special::Finite => unreachable!(),
                };
            }
            let mut coeff = d.coeff.clone();
            if d.exp < 0 {
                let pow = BigInt::from(10).pow((-d.exp) as u32);
                let r = &coeff % &pow;
                coeff = &coeff / &pow;
                if !d.sign && r != BigInt::from(0) {
                    coeff += BigInt::from(1);
                }
            } else if d.exp > 0 {
                coeff *= BigInt::from(10).pow(d.exp as u32);
            }
            let v = if d.sign { -coeff } else { coeff };
            Ok(vm.ctx.new_int(v).into())
        }

        #[pyclassmethod]
        fn from_number(cls: PyTypeRef, number: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            // PEP 3141: from_number converts int/float/Decimal to Decimal.
            // For subclasses, the result is an instance of the subclass.
            if number.downcast_ref::<PyDecimal>().is_some() {
                // If already a Decimal of the exact cls, return as-is.
                if number.class().is(&cls) {
                    return Ok(number);
                }
                // Otherwise reconstruct via str through cls.
                let pd = number.downcast_ref::<PyDecimal>().unwrap();
                let s: PyObjectRef = vm.ctx.new_str(pd.d.to_string()).into();
                let cls_obj: PyObjectRef = cls.to_owned().into();
                return Ok(vm.invoke(&cls_obj, (s,))?);
            }
            if let Some(i) = number.downcast_ref::<PyInt>() {
                let d = _decimal_core::dec_from_int(vm, i);
                let s: PyObjectRef = vm.ctx.new_str(d.to_string()).into();
                let cls_obj: PyObjectRef = cls.to_owned().into();
                return Ok(vm.invoke(&cls_obj, (s,))?);
            }
            if let Some(f) = number.downcast_ref::<PyFloat>() {
                let d = _decimal_core::dec_from_float(f.to_f64());
                let s: PyObjectRef = vm.ctx.new_str(d.to_string()).into();
                let cls_obj: PyObjectRef = cls.to_owned().into();
                return Ok(vm.invoke(&cls_obj, (s,))?);
            }
            Err(vm.new_type_error("from_number requires int or float".to_string()))
        }

        #[pyclassmethod]
        fn from_float(cls: PyTypeRef, number: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            // Exact binary expansion of the float (like CPython's
            // Decimal.from_float). Integers are converted exactly; the
            // _decimal constructor ignores context.
            let d = if let Some(f) = number.downcast_ref::<PyFloat>() {
                _decimal_core::dec_from_float(f.to_f64())
            } else if let Some(i) = number.downcast_ref::<PyInt>() {
                _decimal_core::dec_from_int(vm, i)
            } else {
                return Err(vm.new_type_error(
                    "argument must be an int or float".to_string(),
                ));
            };
            // Build through the (possibly subclassed) cls so MyDecimal.from_float
            // returns a MyDecimal instance. CPython passes the resulting
            // Decimal object to the constructor (subclass __init__ sees a
            // Decimal, not a str).
            let dec_obj: PyObjectRef = PyDecimal::to_py(vm, d);
            let cls_obj: PyObjectRef = cls.to_owned().into();
            let inst = vm.invoke(&cls_obj, (dec_obj,))?;
            Ok(inst)
        }

        #[pymethod]
        fn __reduce__(zelf: PyRef<Self>, vm: &VirtualMachine) -> PyResult {
            // Support pickle: return (Decimal, (str(self),))
            let decimal_type = zelf.class().to_owned();
            let s = vm.ctx.new_str(zelf.d.to_string());
            Ok(vm.ctx.new_tuple(vec![
                decimal_type.into(),
                vm.ctx.new_tuple(vec![s.into()]).into(),
            ]).into())
        }

        #[pymethod]
        fn __copy__(zelf: PyRef<Self>, _vm: &VirtualMachine) -> PyResult {
            // CPython: Decimal is immutable; copy returns the same object.
            Ok(zelf.into())
        }

        #[pymethod]
        fn __deepcopy__(zelf: PyRef<Self>, _memo: PyObjectRef, _vm: &VirtualMachine) -> PyResult {
            // CPython: Decimal is immutable; deepcopy returns the same object.
            Ok(zelf.into())
        }

        #[pymethod]
        fn __round__(&self, ndigits: OptionalArg<PyObjectRef>, vm: &VirtualMachine) -> PyResult {
            let xd = &self.d;
            if xd.is_special() {
                // With ndigits, round(special, n) -> NaN (CPython).
                // Without ndigits, raises ValueError/OverflowError.
                if ndigits.into_option().is_some() {
                    // CPython: round(Inf, n) -> NaN (sign dropped).
                    return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                        sign: false, coeff: xd.coeff.clone(), exp: 0,
                        special: _decimal_core::Special::NaN,
                    }));
                }
                return match xd.special {
                    Special::Infinity => Err(vm.new_overflow_error("cannot convert Infinity to integer".to_string())),
                    Special::NaN | Special::SignalingNaN => Err(vm.new_value_error("cannot convert NaN to integer".to_string())),
                    Special::Finite => unreachable!(),
                };
            }
            let ctx = current_ctx(vm)?;
            if let Some(nd) = ndigits.into_option() {
                let n: isize = nd.try_to_value(vm)?;
                // Negating isize::MIN would overflow; that rounds to an
                // absurdly large exponent, which quantize rejects as out of
                // range (InvalidOperation), matching CPython.
                let exp = n.checked_neg().unwrap_or(isize::MAX);
                let r = _decimal_core::quantize(vm, &ctx, xd, exp)?;
                return Ok(PyDecimal::to_py(vm, r));
            }
            let mut coeff = xd.coeff.clone();
            if xd.exp < 0 {
                let pow = BigInt::from(10).pow((-xd.exp) as u32);
                let (q, r) = (&coeff / &pow, &coeff % &pow);
                let half = &pow / BigInt::from(2);
                let round_up = if &r * BigInt::from(2) > pow { true }
                    else if &r * BigInt::from(2) < pow { false }
                    else { (&q % BigInt::from(2)) == BigInt::from(1) };
                coeff = if round_up { q + BigInt::from(1) } else { q };
            } else if xd.exp > 0 {
                coeff *= BigInt::from(10).pow(xd.exp as u32);
            }
            let v = if xd.sign { -coeff } else { coeff };
            Ok(vm.ctx.new_int(v).into())
        }


        #[pymethod]
        fn to_integral(
            &self,
            args: FuncArgs,
            vm: &VirtualMachine,
        ) -> PyResult {
            // CPython: the optional rounding arg must be a valid rounding mode
            // string or None; anything else is a TypeError.
            if args.args.len() > 1 {
                return Err(vm.new_type_error("optional argument must be a context".to_string()));
            }
            let rounding_str: Option<String> = args
                .get_optional_kwarg("rounding")
                .or_else(|| args.args.get(0).cloned())
                // None means "use the context rounding" (CPython).
                .filter(|v| !v.is(&vm.ctx.none()))
                .map(|v| match v.downcast_ref::<PyStr>() {
                    Some(s) => {
                        let txt = s.as_wtf8().to_string();
                        if Rounding::from_str(&txt).is_none() {
                            return Err(vm.new_type_error(format!(
                                "invalid rounding mode: {txt} (valid: {})",
                                "ROUND_CEILING, ROUND_FLOOR, ROUND_DOWN, ROUND_UP, ROUND_HALF_DOWN, ROUND_HALF_EVEN, ROUND_HALF_UP, ROUND_05UP"
                            )));
                        }
                        Ok(txt)
                    }
                    None => Err(vm.new_type_error("rounding must be a string".to_string())),
                })
                .transpose()?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let mut ctx2 = ctx;
            if let Some(r) = rounding_str {
                ctx2.rounding = Rounding::from_str(&r).unwrap();
            }
            let r = _decimal_core::to_integral(vm, &ctx2, &self.d, false)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn to_integral_value(
            &self,
            args: FuncArgs,
            vm: &VirtualMachine,
        ) -> PyResult {
            // Accepts an optional positional rounding mode (CPython).
            if args.args.len() > 1 {
                return Err(vm.new_type_error("too many arguments".to_string()));
            }
            let rounding_str: Option<String> = args
                .get_optional_kwarg("rounding")
                .or_else(|| args.args.get(0).cloned())
                .filter(|v| !v.is(&vm.ctx.none()))
                .map(|v| match v.downcast_ref::<PyStr>() {
                    Some(s) => {
                        let txt = s.as_wtf8().to_string();
                        if Rounding::from_str(&txt).is_none() {
                            return Err(vm.new_type_error(format!(
                                "invalid rounding mode: {txt}"
                            )));
                        }
                        Ok(txt)
                    }
                    None => Err(vm.new_type_error("rounding must be a string".to_string())),
                })
                .transpose()?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let mut ctx2 = ctx;
            if let Some(r) = rounding_str {
                ctx2.rounding = Rounding::from_str(&r).unwrap();
            }
            let r = _decimal_core::to_integral(vm, &ctx2, &self.d, false)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn to_integral_exact(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            if args.args.len() > 1 {
                return Err(vm.new_type_error("too many arguments".to_string()));
            }
            let rounding_str: Option<String> = args
                .get_optional_kwarg("rounding")
                .or_else(|| args.args.get(0).cloned())
                .filter(|v| !v.is(&vm.ctx.none()))
                .map(|v| match v.downcast_ref::<PyStr>() {
                    Some(s) => {
                        let txt = s.as_wtf8().to_string();
                        if Rounding::from_str(&txt).is_none() {
                            return Err(vm.new_type_error(format!(
                                "invalid rounding mode: {txt}"
                            )));
                        }
                        Ok(txt)
                    }
                    None => Err(vm.new_type_error("rounding must be a string".to_string())),
                })
                .transpose()?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let mut ctx2 = ctx;
            if let Some(r) = rounding_str {
                ctx2.rounding = Rounding::from_str(&r).unwrap();
            }
            let r = _decimal_core::to_integral(vm, &ctx2, &self.d, true)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn to_eng_string(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult<String> {
            let _ = ctx_from_noargs(&args, vm)?;
            let mut s = self.d.to_eng_string();
            if current_capitals(vm)? == 0 {
                s = s.replace('E', "e");
            }
            Ok(s)
        }

        // Logical operations (operate on the decimal digit strings; result
        // keeps the operand's precision width).
        #[pymethod]
        fn logical_and(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let other = args.args.get(0).cloned()
                .ok_or_else(|| vm.new_type_error("logical_and requires an argument".to_string()))?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let b = to_dec_ctx(&other, vm)?;
            let r = logical_binary_op(vm, &ctx, &self.d, &b, "logical_and requires non-negative integers", |x, y| x & y)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn logical_or(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let other = args.args.get(0).cloned()
                .ok_or_else(|| vm.new_type_error("logical_or requires an argument".to_string()))?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let b = to_dec_ctx(&other, vm)?;
            let r = logical_binary_op(vm, &ctx, &self.d, &b, "logical_or requires non-negative integers", |x, y| x | y)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn logical_xor(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let other = args.args.get(0).cloned()
                .ok_or_else(|| vm.new_type_error("logical_xor requires an argument".to_string()))?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let b = to_dec_ctx(&other, vm)?;
            let r = logical_binary_op(vm, &ctx, &self.d, &b, "logical_xor requires non-negative integers", |x, y| x ^ y)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn logical_invert(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let ctx = ctx_from_noargs(&args, vm)?;
            if self.d.special != Special::Finite || self.d.exp != 0 || self.d.sign
                || !self.d.coeff.to_string().chars().all(|c| c == '0' || c == '1')
            {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "logical_invert requires 0/1 digits"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            // Invert within the context precision width (decimal digits).
            let coeff_str = self.d.coeff.to_string();
            let width = ctx.prec.max(coeff_str.len() as isize) as usize;
            let ones: String = "1".repeat(width);
            // Build result digit-wise: result digit = operator applied to operand
            // digits of equal length; 0-pad the shorter operand to width.
            let mut a = coeff_str;
            while a.len() < width { a.insert(0, '0'); }
            let b = &ones;
            let mut out = String::new();
            for (ca, cb) in a.chars().zip(b.chars()) {
                let da = ca.to_digit(10).unwrap_or(0);
                let db = cb.to_digit(10).unwrap_or(0);
                let r = !(da != 0) as u32;  // bitwise NOT of da (only 0/1)
                out.push_str(&r.to_string());
                let _ = db;
            }
            let coeff = out.parse::<BigInt>().unwrap_or(BigInt::from(0));
            Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                sign: false, coeff, exp: 0, special: Special::Finite,
            }))
        }

        #[pymethod]
        fn rotate(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let n_argv = args.args.get(0).cloned()
                .ok_or_else(|| vm.new_type_error("rotate requires an argument".to_string()))?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let n_obj = to_dec_ctx(&n_argv, vm)?;
            let n: i64 = n_obj.coeff.to_string().parse().unwrap_or(0);
            let n = if n_obj.sign { -n } else { n };
            let r = rotate_value(vm, &ctx, &self.d, n)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn scaleb(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let n_argv = args.args.get(0).cloned()
                .ok_or_else(|| vm.new_type_error("scaleb requires an argument".to_string()))?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let n_obj = to_dec_ctx(&n_argv, vm)?;
            // Any NaN operand signals InvalidOperation and yields a quiet NaN
            // (payload from the sNaN operand if present: scaleb(NaN016,
            // sNaN191) -> NaN191; scaleb(Inf, -sNaN) -> -NaN).
            if self.d.special == Special::SignalingNaN || n_obj.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "scaleb with sNaN"));
                }
                let src = if self.d.special == Special::SignalingNaN { &self.d } else { &n_obj };
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: src.sign,
                    coeff: src.coeff.clone(),
                    exp: 0,
                    special: Special::NaN,
                }));
            }
            // A quiet NaN operand (no sNaN) passes through without signaling
            // (scbx021/022: scaleb(NaN,1) -> NaN; scaleb(-NaN,-1) -> -NaN,
            // no flags; payload and sign preserved).
            if self.d.special == Special::NaN || n_obj.special == Special::NaN {
                let src = if self.d.special == Special::NaN { &self.d } else { &n_obj };
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: src.sign,
                    coeff: src.coeff.clone(),
                    exp: 0,
                    special: Special::NaN,
                }));
            }
            // An infinite exponent operand is InvalidOperation too
            // (scbx018: scaleb(10, Infinity) -> NaN Invalid_operation).
            if n_obj.special == Special::Infinity {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "scaleb with infinite exponent"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            if n_obj.exp != 0 {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "scaleb requires an integer"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let n: isize = n_obj.coeff.to_string().parse().unwrap_or(isize::MAX);
            let n = if n_obj.sign { -n } else { n };
            // CPython: liminf/limsup = +/-2*(Emax+prec); out of range is
            // InvalidOperation (scbx127: -2018 at prec 9/Emax 999 -> -2018
            // < -2016 is InvalidOperation; -2016/-2015 are legal underflows).
            // Exponents inside the range flow through apply_context, so a
            // large positive n overflows to Infinity rather than raising.
            let limsup = 2isize.checked_mul(ctx.emax + ctx.prec).unwrap_or(isize::MAX);
            let liminf = -2isize.checked_mul(ctx.emax + ctx.prec).unwrap_or(isize::MIN);
            if n < liminf || n > limsup {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "scaleb exponent out of range"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let result = Dec {
                sign: self.d.sign, coeff: self.d.coeff.clone(), exp: self.d.exp + n,
                special: self.d.special,
            };
            let r = _decimal_core::apply_context(vm, &ctx, result)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn shift(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let n_argv = args.args.get(0).cloned()
                .ok_or_else(|| vm.new_type_error("shift requires an argument".to_string()))?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let n_obj = to_dec_ctx(&n_argv, vm)?;
            // A NaN operand propagates before the count is validated, matching
            // the Context method (decTest shix821: shift(NaN, -Inf) -> NaN
            // with no signal).
            if self.d.special == Special::SignalingNaN || n_obj.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "shift with sNaN"));
                }
                let src = if self.d.special == Special::SignalingNaN { &self.d } else { &n_obj };
                return Ok(PyDecimal::to_py(vm, quiet_snan_payload(&ctx, src)));
            }
            if self.d.special == Special::NaN {
                return Ok(PyDecimal::to_py(vm, self.d.clone()));
            }
            if n_obj.special == Special::NaN {
                return Ok(PyDecimal::to_py(vm, n_obj.clone()));
            }
            if self.d.exp < 0 || n_obj.exp != 0 {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "shift requires integer"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let n: i64 = n_obj.coeff.to_string().parse().unwrap_or(0);
            let n = if n_obj.sign { -n } else { n };
            if !(-(ctx.prec as i64) <= n && n <= ctx.prec as i64) {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "shift amount out of range"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            if self.d.special == Special::Infinity {
                return Ok(PyDecimal::to_py(vm, self.d.clone()));
            }
            // CPython's mpd_qshiftn shifts the coefficient and leaves the
            // exponent alone; any padding to the context precision happens
            // afterwards, in _mpd_cap. Shifting inside a prec-wide digit window
            // (as this used to) instead invents a borrow and reported a rounded
            // zero where ddshi005 expects 1E+16.
            let result = if n >= 0 {
                // The left shift overflows the prec-wide window from the top:
                // CPython keeps the LOW prec digits silently (shift(12345, 4)
                // at prec 5 -> 50000, shift(99999, 1) -> 99990, no flags) -
                // the high digits drop off, there is no rounding and no
                // Rounded/Inexact signal.
                let mut c = &self.d.coeff * BigInt::from(10).pow(n as u32);
                let cd = _decimal_core::coeff_digits_of(&c);
                if cd > ctx.prec as usize {
                    let window = BigInt::from(10).pow(ctx.prec as u32);
                    c = c % window;
                }
                _decimal_core::Dec {
                    sign: self.d.sign,
                    coeff: c,
                    exp: self.d.exp,
                    special: Special::Finite,
                }
            } else {
                // Shift right: drop the last |n| digits of the value, toward
                // zero, and fold them into the exponent (mpd_qshiftr).
                let drop = (-n) as u32;
                let pow10 = BigInt::from(10).pow(drop);
                _decimal_core::Dec {
                    sign: self.d.sign,
                    coeff: &self.d.coeff / &pow10,
                    exp: self.d.exp + n as isize,
                    special: Special::Finite,
                }
            };
            // decTest shift: no case carries any signal (shix...: exact
            // coefficient window operation). Return as-is.
            Ok(PyDecimal::to_py(vm, result))
        }

        #[pymethod]
        fn fma(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            // self.fma(other, third, context=) ; other=/third= keywords.
            if args.args.len() > 2 {
                return Err(vm.new_type_error("fma takes at most two positional arguments".to_string()));
            }
            let get = |key: &str| -> Option<PyObjectRef> {
                args.get_optional_kwarg(key).or_else(|| None)
            };
            let other = get("other").or_else(|| args.args.get(0).cloned()).ok_or_else(|| {
                vm.new_type_error("fma requires other and third".to_string())
            })?;
            let third = get("third").or_else(|| args.args.get(1).cloned()).ok_or_else(|| {
                vm.new_type_error("fma requires other and third".to_string())
            })?;
            let other_d = to_dec_ctx(&other, vm)?;
            let third_d = to_dec_ctx(&third, vm)?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            if let Some(r) = _decimal_core::fma_special(vm, &ctx, &self.d, &other_d, &third_d)? {
                return Ok(PyDecimal::to_py(vm, r));
            }
            // The spec's fma is a SINGLE rounding of the exact x*y + z. The
            // old chain rounded x*y to the context precision first, and the
            // double rounding cost one ulp (fma0201: 888565290 * 1557.96930
            // - 86087.7578 -> 1.38435736E+12, we gave ...35E+12). The
            // product's infinities are built here so the add's special path
            // handles Inf +- Inf (fmax0801: Inf Inf -Inf -> NaN
            // Invalid_operation); 0 * Inf signals through the mul's rule.
            let prod = if self.d.special == Special::Infinity
                || other_d.special == Special::Infinity
            {
                let x_inf = self.d.special == Special::Infinity;
                let y_inf = other_d.special == Special::Infinity;
                let x_zero = !x_inf && self.d.coeff == BigInt::from(0);
                let y_zero = !y_inf && other_d.coeff == BigInt::from(0);
                if (x_inf && y_zero) || (y_inf && x_zero) {
                    ctx.set_flag(vm, "InvalidOperation")?;
                    if ctx.trap(vm, "InvalidOperation")? {
                        return Err(new_decimal_error(
                            vm,
                            "InvalidOperation",
                            "0 * Infinity",
                        ));
                    }
                    _decimal_core::Dec {
                        sign: false,
                        coeff: BigInt::from(0),
                        exp: 0,
                        special: Special::NaN,
                    }
                } else {
                    _decimal_core::Dec {
                        sign: self.d.sign != other_d.sign,
                        coeff: BigInt::from(0),
                        exp: 0,
                        special: Special::Infinity,
                    }
                }
            } else {
                _decimal_core::Dec {
                    sign: self.d.sign != other_d.sign,
                    coeff: &self.d.coeff * &other_d.coeff,
                    exp: self.d.exp + other_d.exp,
                    special: Special::Finite,
                }
            };
            let r = _decimal_core::dec_add(&ctx, vm, &prod, &third_d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn remainder_near(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let other = args
                .args
                .get(0)
                .cloned()
                .ok_or_else(|| vm.new_type_error("remainder_near requires an argument".to_string()))?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let b = to_dec_ctx(&other, vm)?;
            // A NaN operand propagates first, and a zero divisor is always
            // InvalidOperation + NaN (never Division_by_zero) - see the
            // Context method for the decTest cases (rmnx101-120).
            if self.d.special == Special::SignalingNaN || b.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "Division_undefined"));
                }
                let src = if self.d.special == Special::SignalingNaN { &self.d } else { &b };
                return Ok(PyDecimal::to_py(vm, quiet_snan_payload(&ctx, src)));
            }
            if self.d.special == Special::NaN {
                return Ok(PyDecimal::to_py(vm, self.d.clone()));
            }
            if b.special == Special::NaN {
                return Ok(PyDecimal::to_py(vm, b.clone()));
            }
            if b.special == Special::Finite && b.coeff == BigInt::from(0) {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "Division_undefined"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            // An INFINITE divisor: x / Infinity = 0, so n = 0 and the result
            // is x itself for a FINITE x (rmnx688). An INFINITE x with an
            // infinite divisor is Inf/Inf -> Invalid (rmnx680).
            if b.special == Special::Infinity {
                if self.d.special == Special::Infinity {
                    ctx.set_flag(vm, "InvalidOperation")?;
                    if ctx.trap(vm, "InvalidOperation")? {
                        return Err(new_decimal_error(vm, "InvalidOperation", "Inf % Inf"));
                    }
                    return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                        sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                    }));
                }
                let r = _decimal_core::apply_context(vm, &ctx, self.d.clone())?;
                return Ok(PyDecimal::to_py(vm, r));
            }
            // CPython: if the exponent difference is >= prec+1 the quotient is
            // too large to compute (abs(self/other) > 10**prec) -> DivisionImpossible.
            let adj = |d: &Dec| d.exp + _decimal_core::coeff_digits_of(&d.coeff) as isize - 1;
            if self.d.special == Special::Infinity
                || (self.d.special == Special::Finite && b.special == Special::Finite
                    && adj(&self.d) - adj(&b) >= ctx.prec + 1)
            {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "DivisionImpossible"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            // r = self - b * n where n = round(self/b) to nearest int (half-even).
            // Intermediate division uses extra precision so rounding is exact.
            let wide = Ctx::from_parts(50, Rounding::HalfEven, 999999999, -999999999, 0,
                vm.ctx.new_dict(), vm.ctx.new_dict());
            let div = _decimal_core::dec_div(&wide, vm, &self.d, &b)?;
            let mut coeff = div.coeff.clone();
            let mut exp = div.exp;
            if exp < 0 {
                // Fast path for an ultra-tiny quotient (rmnx270:
                // remaindernear 1 1e999999999 -> 1): the shift far exceeds the
                // coefficient's digit count, so the integer part is 0 and the
                // remainder is the whole coefficient; no tie is possible,
                // and building 10**shift would materialise a billion-digit
                // number. round to nearest (half-even): n = 0.
                let shift = (-exp) as usize;
                let digits = _decimal_core::coeff_digits_of(&coeff) as usize;
                if shift > digits {
                    coeff = BigInt::from(0);
                } else {
                    let pow = BigInt::from(10).pow(shift as u32);
                    let (q, r) = (&coeff / &pow, &coeff % &pow);
                    let tie = &r * BigInt::from(2) == pow;
                    let round_up = (&r * BigInt::from(2) > pow)
                        || (tie && (&q % BigInt::from(2)) == BigInt::from(1));
                    coeff = if round_up { q + BigInt::from(1) } else { q };
                }
                exp = 0;
            } else if exp > 0 {
                // A quotient above 1E+0 (remaindernear(1E+2, 3) -> 33.33E+0
                // style): fold the positive exponent into the coefficient so
                // n is the integer quotient.
                coeff *= BigInt::from(10).pow(exp as u32);
                exp = 0;
            }
            // After rounding to the nearest integer the quotient must still fit
            // the context precision: decTest rmnx076-080 - 999999999 % 1 is
            // fine (n = 999999999, 9 digits at prec 9) but 999999999.5 % 1
            // rounds to n = 1000000000, 10 digits -> Division_impossible.
            if _decimal_core::coeff_digits_of(&coeff) > ctx.prec as usize {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "DivisionImpossible"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let n = Dec { sign: div.sign, coeff, exp: 0, special: Special::Finite };
            let prod = _decimal_core::dec_mul(&wide, vm, &b, &n)?;
            let mut result = _decimal_core::dec_sub(&wide, vm, &self.d, &prod)?;
            // GDA: the zero-sign of remainder_near follows the dividend
            // (rmnx134: remaindernear -0 1 -> -0; remaindernear 0 1 -> 0).
            if result.is_zero() {
                result.sign = self.d.sign;
            }
            let r = _decimal_core::apply_context(vm, &ctx, result)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn as_integer_ratio(&self, vm: &VirtualMachine) -> PyResult {
            let d = &self.d;
            if d.is_special() {
                return match d.special {
                    Special::Infinity => Err(vm.new_overflow_error(
                        "cannot convert Infinity to integer ratio".to_string(),
                    )),
                    Special::NaN | Special::SignalingNaN => Err(vm.new_value_error(
                        "cannot convert NaN to integer ratio".to_string(),
                    )),
                    Special::Finite => unreachable!(),
                };
            }
            // Strip trailing zeros so p and q are coprime.
            let mut coeff = d.coeff.clone();
            let mut exp = d.exp;
            if coeff == BigInt::from(0) {
                return Ok(vm.ctx.new_tuple(vec![
                    vm.ctx.new_int(BigInt::from(0)).into(),
                    vm.ctx.new_int(BigInt::from(1)).into(),
                ]).into());
            }
            // Reduce by removing all factors of 2 and 5 from the denominator
            // that also divide the numerator.
            let den_pow = if exp < 0 { (-exp) as u32 } else { 0 };
            let mut num = coeff.clone();
            let mut den = BigInt::from(1);
            if exp < 0 {
                den = BigInt::from(10).pow(den_pow);
                // Strip common factors of 10 (2*5) from both.
                while (&num % BigInt::from(2)) == BigInt::from(0)
                    && (&den % BigInt::from(2)) == BigInt::from(0)
                {
                    num /= BigInt::from(2);
                    den /= BigInt::from(2);
                }
                while (&num % BigInt::from(5)) == BigInt::from(0)
                    && (&den % BigInt::from(5)) == BigInt::from(0)
                {
                    num /= BigInt::from(5);
                    den /= BigInt::from(5);
                }
            } else if exp > 0 {
                num *= BigInt::from(10).pow(exp as u32);
            }
            let n = if d.sign { -num } else { num };
            Ok(vm.ctx.new_tuple(vec![vm.ctx.new_int(n).into(), vm.ctx.new_int(den).into()]).into())
        }

        #[pymethod]
        fn canonical(zelf: PyRef<Self>) -> PyDecimal {
            PyDecimal { d: zelf.d.clone() }
        }

        #[pygetset]
        fn real(zelf: PyRef<Self>) -> PyDecimal {
            PyDecimal { d: zelf.d.clone() }
        }

        #[pygetset]
        fn imag(&self, vm: &VirtualMachine) -> PyResult {
            Ok(PyDecimal::to_py(vm, _decimal_core::Dec::zero()))
        }

        #[pymethod]
        fn conjugate(zelf: PyRef<Self>) -> PyDecimal {
            PyDecimal { d: zelf.d.clone() }
        }

    }

    impl Constructor for PyDecimal {
        type Args = FuncArgs;

        fn py_new(_cls: &Py<PyType>, args: FuncArgs, vm: &VirtualMachine) -> PyResult<Self> {
            // CPython: Decimal(value[, context]) — the second positional may be
            // a Context; anything more or a non-Context second arg -> TypeError.
            if args.args.len() > 2 {
                return Err(vm.new_type_error(
                    "Decimal() accepts at most 2 positional arguments".to_string(),
                ));
            }
            if args.args.len() == 2 && args.args[1].downcast_ref::<PyContext>().is_none() {
                return Err(vm.new_type_error(
                    "Decimal() takes at most 2 arguments (value and context)".to_string(),
                ));
            }
            // Support keyword args: Decimal(value=..., context=...)
            let value = args.get_optional_kwarg("value")
                .or_else(|| args.args.get(0).cloned())
                .unwrap_or_else(|| vm.ctx.new_int(0).into());
            // context keyword overrides the current context (CPython uses it for
            // flag/precision application during construction). A positional 2nd
            // arg is also accepted as the context.
            let context_kw = args
                .get_optional_kwarg("context")
                .or_else(|| args.args.get(1).cloned());
            // Accept a tuple OR list as the (sign, digits, exponent) triple.
            let triple: Vec<PyObjectRef> = if let Some(tuple) = value.downcast_ref::<PyTuple>() {
                tuple.as_slice().to_vec()
            } else if let Some(lst) = value.downcast_ref::<crate::vm::builtins::PyList>() {
                lst.borrow_vec().to_vec()
            } else {
                Vec::new()
            };
            if !triple.is_empty() {
                let d = Self::from_tuple(&triple, vm)?;
                // CPython: Decimal(tuple) raises InvalidOperation when the
                // exponent exceeds the representable range (MAX_EMAX /
                // MIN_ETINY).
                if d.exp > 999_999_999_999_999_999 || d.exp < -1_999_999_999_999_999_997 {
                    let ctx = match &context_kw {
                        Some(c) if c.downcast_ref::<PyContext>().is_some() => c
                            .downcast_ref::<PyContext>()
                            .unwrap()
                            .ctx(),
                        _ => current_ctx(vm)?,
                    };
                    ctx.set_flag(vm, "InvalidOperation")?;
                    if ctx.trap(vm, "InvalidOperation")? {
                        return Err(new_decimal_error(vm, "InvalidOperation", "exponent out of range"));
                    }
                    return Ok(PyDecimal {
                        d: _decimal_core::Dec {
                            sign: false,
                            coeff: BigInt::from(0),
                            exp: 0,
                            special: _decimal_core::Special::NaN,
                        },
                    });
                }
                return Ok(PyDecimal { d });
            }
            // CPython 3.11+: invalid string like 'ugly' constructs a
            // quiet NaN (with InvalidOperation flag) instead of raising.
            // But TypeError (e.g. Decimal(None)) must still propagate.
            // Decimal(float) is an implicit float conversion: it sets the
            // FloatOperation flag (explicit Decimal.from_float does not).
            let mut ctx = match &context_kw {
                Some(c) => {
                    let pc = c.downcast_ref::<PyContext>().ok_or_else(|| {
                        vm.new_type_error("context must be a Context object".to_string())
                    })?;
                    pc.ctx()
                }
                None => current_ctx(vm)?,
            };
            if value.downcast_ref::<PyFloat>().is_some() {
                ctx.set_flag(vm, "FloatOperation")?;
                if ctx.trap(vm, "FloatOperation")? {
                    return Err(new_decimal_error(
                        vm,
                        "FloatOperation",
                        "float conversion is not safe for Decimal",
                    ));
                }
            }
            let d = match to_dec(&value, vm) {
                Ok(d) => d,
                Err(e) => {
                    // Only InvalidOperation (string parse failure) -> NaN.
                    // TypeError and others propagate as-is.
                    let is_io = e.class().is(&exc().invalid_operation);
                    if !is_io {
                        return Err(e);
                    }
                    let _ = ctx.set_flag(vm, "InvalidOperation");
                    // If the InvalidOperation trap is on, raise the error.
                    if ctx.trap(vm, "InvalidOperation")? {
                        return Err(e);
                    }
                    Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN }
                }
            };
            Ok(PyDecimal { d })
        }
    }

    impl Representable for PyDecimal {
        fn repr_str(zelf: &Py<Self>, _vm: &VirtualMachine) -> PyResult<String> {
            Ok(format!("Decimal('{}')", zelf.d.to_string()))
        }
    }

    impl Comparable for PyDecimal {
        fn cmp(
            zelf: &Py<Self>,
            other: &PyObject,
            op: PyComparisonOp,
            vm: &VirtualMachine,
        ) -> PyResult<PyComparisonValue> {
            // Comparing Decimal with float is an implicit float conversion.
            // CPython: equality (==/!=) sets the flag but never raises (so
            // sets/dicts work); ordering (< <= > >=) raises if trapped.
            if other.downcast_ref::<PyFloat>().is_some() {
                let ctx = current_ctx(vm)?;
                ctx.set_flag(vm, "FloatOperation")?;
                if matches!(op, PyComparisonOp::Lt | PyComparisonOp::Le | PyComparisonOp::Gt | PyComparisonOp::Ge)
                    && ctx.trap(vm, "FloatOperation")?
                {
                    return Err(new_decimal_error(
                        vm,
                        "FloatOperation",
                        "float conversion is not safe for Decimal",
                    ));
                }
            }
            let b = match to_dec(other, vm) {
                Ok(b) => b,
                Err(_) => {
                    // Fraction comparison: exact via numerator/denominator.
                    // self (Decimal) vs other (Fraction) — compare the exact
                    // rational self.d against num/den.
                    // Specials first: Infinity/NaN never convert to Dec here
                    // (Fraction is not a Decimal), but compare naturally.
                    let is_special = zelf.d.special != Special::Finite;
                    if is_special {
                        if let (Ok(num), Ok(den)) = (
                            other.get_attr("numerator", vm),
                            other.get_attr("denominator", vm),
                        ) {
                            let num_is_int = num.downcast_ref::<PyInt>().is_some();
                            let den_is_int = den.downcast_ref::<PyInt>().is_some();
                            if num_is_int && den_is_int {
                                if matches!(
                                    zelf.d.special,
                                    Special::NaN | Special::SignalingNaN
                                ) {
                                    // CPython: NaN ordering comparisons with a
                                    // non-Decimal signal InvalidOperation; only
                                    // equality is allowed (always false/true).
                                    if matches!(
                                        op,
                                        PyComparisonOp::Lt | PyComparisonOp::Le
                                            | PyComparisonOp::Gt | PyComparisonOp::Ge
                                    ) {
                                        let ctx = current_ctx(vm)?;
                                        ctx.set_flag(vm, "InvalidOperation")?;
                                        if ctx.trap(vm, "InvalidOperation")? {
                                            return Err(new_decimal_error(
                                                vm,
                                                "InvalidOperation",
                                                "order comparison with NaN",
                                            ));
                                        }
                                        return Ok(PyComparisonValue::Implemented(false));
                                    }
                                    let result = matches!(op, PyComparisonOp::Ne);
                                    return Ok(PyComparisonValue::Implemented(result));
                                }
                                // Infinity ±: always greater/less than any
                                // finite rational.
                                let self_pos = !zelf.d.sign;
                                let result = match op {
                                    PyComparisonOp::Lt => !self_pos,
                                    PyComparisonOp::Le => !self_pos,
                                    PyComparisonOp::Gt => self_pos,
                                    PyComparisonOp::Ge => self_pos,
                                    PyComparisonOp::Eq => false,
                                    PyComparisonOp::Ne => true,
                                };
                                return Ok(PyComparisonValue::Implemented(result));
                            }
                        }
                    }
                    if let (Ok(num), Ok(den)) = (
                        other.get_attr("numerator", vm),
                        other.get_attr("denominator", vm),
                    ) {
                        let num: BigInt = num
                            .downcast_ref::<PyInt>()
                            .map(|i| i.as_bigint().clone())
                            .unwrap_or_default();
                        let den: BigInt = den
                            .downcast_ref::<PyInt>()
                            .map(|i| i.as_bigint().clone())
                            .unwrap_or_default();
                        if den != BigInt::from(0) {
                            // Compare exact rationals:
                            //   self = s * c * 10^e  vs  num/den
                            // Guard: for astronomically large |e|, materializing
                            // 10^|e| would be unbounded, so the comparison is
                            // decided by sign/zero dominance instead.
                            let s = if zelf.d.sign { -1i8 } else { 1i8 };
                            // Fractions normalize denominator positive, so the
                            // sign of num determines the fraction's sign.
                            let frac_pos = num >= BigInt::from(0);
                            if zelf.d.exp.abs() > 1_000_000 {
                                let self_zero = zelf.d.coeff == BigInt::from(0);
                                let self_pos = s > 0;
                                // Fraction is zero (num==0): compare self against 0.
                                if num == BigInt::from(0) {
                                    let result = match op {
                                        PyComparisonOp::Eq => self_zero,
                                        PyComparisonOp::Ne => !self_zero,
                                        PyComparisonOp::Lt => !self_pos && !self_zero, // self < 0 iff negative
                                        PyComparisonOp::Le => !self_pos || self_zero,
                                        PyComparisonOp::Gt => self_pos && !self_zero, // self > 0 iff positive
                                        PyComparisonOp::Ge => self_pos || self_zero,
                                    };
                                    return Ok(PyComparisonValue::Implemented(result));
                                }
                                // Fraction is a finite nonzero rational.
                                // |self| is either ~0 (e << 0) or unbounded
                                // (e >> 0), so magnitude dominance decides:
                                //   tiny self: |self| < |frac| -> Lt iff signs
                                //     put self left: Lt = frac_pos (see cases)
                                //   huge self: |self| > |frac| -> Lt iff self<0
                                let result = if self_zero {
                                    // self == 0 vs nonzero frac
                                    match op {
                                        PyComparisonOp::Eq => false,
                                        PyComparisonOp::Ne => true,
                                        PyComparisonOp::Lt => frac_pos, // 0 < positive
                                        PyComparisonOp::Le => frac_pos,
                                        PyComparisonOp::Gt => !frac_pos, // 0 > negative
                                        PyComparisonOp::Ge => !frac_pos,
                                    }
                                } else if zelf.d.exp < 0 {
                                    // self tiny: |self| < |frac|, so self is on
                                    // the same side of zero but closer.
                                    // both pos: self < frac (T); both neg: self > frac (F);
                                    // mixed: negative < positive.
                                    match op {
                                        PyComparisonOp::Eq => false,
                                        PyComparisonOp::Ne => true,
                                        PyComparisonOp::Lt => frac_pos,
                                        PyComparisonOp::Le => frac_pos,
                                        PyComparisonOp::Gt => !frac_pos,
                                        PyComparisonOp::Ge => !frac_pos,
                                    }
                                } else {
                                    // self huge: |self| > |frac|.
                                    match op {
                                        PyComparisonOp::Eq => false,
                                        PyComparisonOp::Ne => true,
                                        PyComparisonOp::Lt => !self_pos, // self < frac iff self negative
                                        PyComparisonOp::Le => !self_pos,
                                        PyComparisonOp::Gt => self_pos, // self > frac iff self positive
                                        PyComparisonOp::Ge => self_pos,
                                    }
                                };
                                return Ok(PyComparisonValue::Implemented(result));
                            }
                            let (lhs, rhs) = if zelf.d.exp >= 0 {
                                let self_num = BigInt::from(s)
                                    * &zelf.d.coeff
                                    * BigInt::from(10).pow(zelf.d.exp as u32);
                                (self_num * &den, num)
                            } else {
                                let pow = BigInt::from(10).pow((-zelf.d.exp) as u32);
                                let self_num = BigInt::from(s) * &zelf.d.coeff;
                                (self_num * &den, num * pow)
                            };
                            let r = lhs.cmp(&rhs);
                            let result = match op {
                                PyComparisonOp::Lt => r.is_lt(),
                                PyComparisonOp::Le => r.is_le(),
                                PyComparisonOp::Gt => r.is_gt(),
                                PyComparisonOp::Ge => r.is_ge(),
                                PyComparisonOp::Eq => r.is_eq(),
                                PyComparisonOp::Ne => r.is_ne(),
                            };
                            return Ok(PyComparisonValue::Implemented(result));
                        }
                    }
                    // Complex comparison: only meaningful when the imaginary
                    // part is zero; then compare against the real part.
                    if let Some(cx) = other.downcast_ref::<PyComplex>() {
                        let cv = cx.to_complex64();
                        let imag = cv.im;
                        if imag == 0.0 {
                            let real = cv.re;
                            let b = _decimal_core::dec_from_float(real);
                            // Only equality/inequality are well-defined with
                            // complex; ordering returns NotImplemented.
                            if matches!(op, PyComparisonOp::Eq | PyComparisonOp::Ne) {
                                let r = dec_cmp(&zelf.d, &b);
                                let result = match op {
                                    PyComparisonOp::Eq => r == 0,
                                    _ => r != 0,
                                };
                                return Ok(PyComparisonValue::Implemented(result));
                            }
                        }
                    }
                    return Ok(PyComparisonValue::NotImplemented);
                }
            };
            // sNaN comparisons signal InvalidOperation. If the trap is on,
            // raise; otherwise just set the flag and return the result.
            let ssnan = zelf.d.special == Special::SignalingNaN
                || b.special == Special::SignalingNaN;
            if ssnan {
                let ctx = current_ctx(vm)?;
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "comparison with sNaN"));
                }
            }
            let nan = zelf.d.special == Special::NaN
                || zelf.d.special == Special::SignalingNaN
                || b.special == Special::NaN
                || b.special == Special::SignalingNaN;
            // Ordering comparisons with any NaN signal InvalidOperation
            // (CPython raises when trapped); only ==/!= are non-signaling.
            if nan
                && matches!(
                    op,
                    PyComparisonOp::Lt | PyComparisonOp::Le | PyComparisonOp::Gt | PyComparisonOp::Ge
                )
            {
                let ctx = current_ctx(vm)?;
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "order comparison with NaN"));
                }
                return Ok(PyComparisonValue::Implemented(false));
            }
            let r = dec_cmp(&zelf.d, &b);
            let result = match op {
                PyComparisonOp::Lt => !nan && r < 0,
                PyComparisonOp::Le => !nan && r <= 0,
                PyComparisonOp::Gt => !nan && r > 0,
                PyComparisonOp::Ge => !nan && r >= 0,
                PyComparisonOp::Eq => !nan && r == 0,
                PyComparisonOp::Ne => nan || r != 0,
            };
            Ok(PyComparisonValue::Implemented(result))
        }
    }

    impl Hashable for PyDecimal {
        fn hash(zelf: &Py<Self>, vm: &VirtualMachine) -> PyResult<hash::PyHash> {
            let d = &zelf.d;
            if d.is_special() {
                return match d.special {
                    // CPython: hash(sNaN) raises TypeError; hash(qNaN) is
                    // the identity-based object hash.
                    Special::SignalingNaN => Err(vm.new_type_error(
                        "unhashable type: 'decimal.Decimal'".to_string(),
                    )),
                    Special::NaN => Ok(zelf.as_object().get_id() as hash::PyHash),
                    // CPython: hash matches the float hash (±inf).
                    Special::Infinity => {
                        let f: f64 = if d.sign { f64::NEG_INFINITY } else { f64::INFINITY };
                        let float_obj: PyObjectRef = vm.ctx.new_float(f).into();
                        let v = vm.call_method(&float_obj, "__hash__", ())?;
                        v.try_to_value::<hash::PyHash>(vm)
                    }
                    Special::Finite => unreachable!(),
                };
            }
            let integral = d.exp >= 0
                || {
                    let pow = BigInt::from(10).pow((-d.exp) as u32);
                    (&d.coeff % &pow) == BigInt::from(0)
                };
            if integral {
                let mut v = if d.exp < 0 {
                    let pow = BigInt::from(10).pow((-d.exp) as u32);
                    &d.coeff / &pow
                } else {
                    &d.coeff * BigInt::from(10).pow(d.exp as u32)
                };
                if d.sign {
                    v = -v;
                }
                Ok(hash::fix_sentinel(hash::hash_bigint(&v)))
            } else {
                // CPython: hash(Decimal('3.14')) == hash(3.14).
                // Use the float hash of the decimal's value.
                let f = d.to_string().parse::<f64>().unwrap_or(0.0);
                // delegate to float.__hash__ via the VM
                let float_obj: PyObjectRef = vm.ctx.new_float(f).into();
                let hash_val = vm.call_method(&float_obj, "__hash__", ())?;
                hash_val.try_to_value::<hash::PyHash>(vm)
            }
        }
    }

    fn binop_num(
        a: &PyObject,
        b: &PyObject,
        vm: &VirtualMachine,
        f: fn(&Ctx, &VirtualMachine, &Dec, &Dec) -> PyResult<Dec>,
    ) -> PyResult {
        // CPython rules for Decimal mixed arithmetic: Decimal op int works
        // (int converts exactly), but Decimal op float/str is a TypeError.
        // Return NotImplemented so the other operand gets a chance, which
        // ultimately surfaces the same TypeError.
        let can_convert = |o: &PyObject| -> bool {
            o.downcast_ref::<PyDecimal>().is_some()
                || o.downcast_ref::<PyInt>().is_some()
        };
        if !can_convert(a) || !can_convert(b) {
            return Ok(vm.ctx.not_implemented());
        }
        let (ad, bd) = if let Some(pd) = a.downcast_ref::<PyDecimal>() {
            (pd.d.clone(), match to_dec(b, vm) {
                Ok(d) => d,
                Err(_) => return Ok(vm.ctx.not_implemented()),
            })
        } else if let Some(pd) = b.downcast_ref::<PyDecimal>() {
            (match to_dec(a, vm) {
                Ok(d) => d,
                Err(_) => return Ok(vm.ctx.not_implemented()),
            }, pd.d.clone())
        } else {
            return Ok(vm.ctx.not_implemented());
        };
        let ctx = current_ctx(vm)?;
        let r = f(&ctx, vm, &ad, &bd)?;
        Ok(PyDecimal::to_py(vm, r))
    }

    impl AsNumber for PyDecimal {
        fn as_number() -> &'static PyNumberMethods {
            static AS_NUMBER: PyNumberMethods = PyNumberMethods {
                add: Some(|a, b, vm| binop_num(a, b, vm, _decimal_core::dec_add)),
                subtract: Some(|a, b, vm| binop_num(a, b, vm, _decimal_core::dec_sub)),
                multiply: Some(|a, b, vm| binop_num(a, b, vm, _decimal_core::dec_mul)),
                true_divide: Some(|a, b, vm| binop_num(a, b, vm, _decimal_core::dec_div)),
                floor_divide: Some(|a, b, vm| binop_num(a, b, vm, _decimal_core::dec_floordiv)),
                remainder: Some(|a, b, vm| binop_num(a, b, vm, _decimal_core::dec_mod)),
                power: Some(|a, b, c, vm| {
                    if !vm.is_none(c) {
                        // ternary power: a ** b % m; Decimal can be the base,
                        // exponent, or modulus (CPython dispatches on any of them)
                        let can = |o: &PyObject| o.downcast_ref::<PyDecimal>().is_some() || o.downcast_ref::<PyInt>().is_some();
                        if !can(a) || !can(b) {
                            return Ok(vm.ctx.not_implemented());
                        }
                        let c_obj = c.to_owned();
                        let (ad, bd) = if let Some(pa) = a.downcast_ref::<PyDecimal>() {
                            (pa.d.clone(), match to_dec(b, vm) {
                                Ok(d) => d,
                                Err(_) => return Ok(vm.ctx.not_implemented()),
                            })
                        } else if let Some(pb) = b.downcast_ref::<PyDecimal>() {
                            (match to_dec(a, vm) {
                                Ok(d) => d,
                                Err(_) => return Ok(vm.ctx.not_implemented()),
                            }, pb.d.clone())
                        } else {
                            // Neither a nor b is Decimal: only the modulus is.
                            // Convert both ints to Decimal via to_dec.
                            (match to_dec(a, vm) {
                                Ok(d) => d,
                                Err(_) => return Ok(vm.ctx.not_implemented()),
                            }, match to_dec(b, vm) {
                                Ok(d) => d,
                                Err(_) => return Ok(vm.ctx.not_implemented()),
                            })
                        };
                        let md = to_dec_ctx(&c_obj, vm)?;
                        let ctx = current_ctx(vm)?;
                        let r = _decimal_core::dec_pow_mod(&ctx, vm, &ad, &bd, &md)?;
                        return Ok(PyDecimal::to_py(vm, r));
                    }
                    binop_num(a, b, vm, _decimal_core::dec_pow_int)
                }),
                divmod: Some(|a, b, vm| {
                    // return (a//b, a%b)
                    if !a.downcast_ref::<PyDecimal>().is_some() && !b.downcast_ref::<PyDecimal>().is_some() {
                        return Ok(vm.ctx.not_implemented());
                    }
                    let (ad, bd) = if let Some(pd) = a.downcast_ref::<PyDecimal>() {
                        (pd.d.clone(), match to_dec(b, vm) {
                            Ok(d) => d,
                            Err(_) => return Ok(vm.ctx.not_implemented()),
                        })
                    } else if let Some(pd) = b.downcast_ref::<PyDecimal>() {
                        (match to_dec(a, vm) {
                            Ok(d) => d,
                            Err(_) => return Ok(vm.ctx.not_implemented()),
                        }, pd.d.clone())
                    } else {
                        return Ok(vm.ctx.not_implemented());
                    };
                    let ctx = current_ctx(vm)?;
                    // A quotient needing more than prec digits is CPython's
                    // DivisionImpossible, reported through InvalidOperation.
                    let divided = _decimal_core::floor_div_mod(&ad, &bd, ctx.prec);
                    // Set InvalidOperation flag if either operand was NaN.
                    // Set flags per CPython divmod semantics:
                    //   NaN/Inf operand or 0/0   -> InvalidOperation
                    //   nonzero / 0              -> InvalidOperation + DivisionByZero
                    //   quotient too large       -> InvalidOperation
                    if ad.is_special() || bd.is_special() || bd.is_zero() || divided.is_none() {
                        ctx.set_flag(vm, "InvalidOperation")?;
                    }
                    if !ad.is_special() && !bd.is_special() && !ad.is_zero() && bd.is_zero() {
                        ctx.set_flag(vm, "DivisionByZero")?;
                    }
                    // InvalidOperation takes precedence over DivisionByZero
                    // when both are trapped (CPython).
                    if ctx.trap(vm, "InvalidOperation")? {
                        return Err(new_decimal_error(vm, "InvalidOperation", "invalid operation"));
                    }
                    if ctx.trap(vm, "DivisionByZero")? {
                        return Err(new_decimal_error(vm, "DivisionByZero", "division by zero"));
                    }
                    let (floor, m) = match divided {
                        Some(rm) => rm,
                        None => {
                            let nan = _decimal_core::Dec {
                                sign: false,
                                coeff: BigInt::from(0),
                                exp: 0,
                                special: Special::NaN,
                            };
                            (nan.clone(), nan)
                        }
                    };
                    Ok(vm.ctx.new_tuple(vec![
                        PyDecimal::to_py(vm, floor),
                        PyDecimal::to_py(vm, m),
                    ]).into())
                }),
                // Unlike copy_negate/copy_abs, the unary operators round to the
                // current context's precision and re-check its Emin/Emax.
                negative: Some(|num, vm| {
                    let pd = PyDecimal::number_downcast(num);
                    let mut d = pd.d.clone();
                    d.sign = !d.sign;
                    let ctx = current_ctx(vm)?;
                    let d = _decimal_core::apply_context(vm, &ctx, d)?;
                    Ok(PyDecimal::to_py(vm, d))
                }),
                positive: Some(|num, vm| {
                    let pd = PyDecimal::number_downcast(num);
                    let ctx = current_ctx(vm)?;
                    let d = _decimal_core::apply_context(vm, &ctx, pd.d.clone())?;
                    Ok(PyDecimal::to_py(vm, d))
                }),
                absolute: Some(|num, vm| {
                    let pd = PyDecimal::number_downcast(num);
                    let mut d = pd.d.clone();
                    d.sign = false;
                    let ctx = current_ctx(vm)?;
                    let d = _decimal_core::apply_context(vm, &ctx, d)?;
                    Ok(PyDecimal::to_py(vm, d))
                }),
                boolean: Some(|num, _vm| {
                    let pd = PyDecimal::number_downcast(num);
                    Ok(!pd.d.is_zero()
                        && pd.d.special != Special::NaN
                        && pd.d.special != Special::SignalingNaN)
                }),
                int: Some(|num, vm| {
                    let pd = PyDecimal::number_downcast(num);
                    if pd.d.is_special() {
                        // CPython: int(Decimal('Infinity')) raises OverflowError,
                        // int(Decimal('NaN')) raises ValueError.
                        if pd.d.special == Special::Infinity {
                            return Err(vm.new_overflow_error(
                                "cannot convert Infinity to integer".to_string(),
                            ));
                        }
                        return Err(vm.new_value_error(
                            "cannot convert NaN to integer".to_string(),
                        ));
                    }
                    let mut coeff = pd.d.coeff.clone();
                    if pd.d.exp < 0 {
                        let pow = BigInt::from(10).pow((-pd.d.exp) as u32);
                        coeff = &coeff / &pow;
                    } else if pd.d.exp > 0 {
                        coeff *= BigInt::from(10).pow(pd.d.exp as u32);
                    }
                    if pd.d.sign {
                        coeff = -coeff;
                    }
                    Ok(vm.ctx.new_int(coeff).into())
                }),
                float: Some(|num, vm| {
                    let pd = PyDecimal::number_downcast(num);
                    // Explicit sNaN -> ValueError (CPython). Preserve the
                    // sign bit for -NaN/-Infinity.
                    let f = match pd.d.special {
                        Special::SignalingNaN => {
                            return Err(vm.new_value_error(
                                "cannot convert signaling NaN to float".to_string(),
                            ))
                        }
                        Special::NaN => {
                            let mut f = f64::NAN;
                            if pd.d.sign {
                                f = -f;
                            }
                            f
                        }
                        Special::Infinity => {
                            if pd.d.sign { f64::NEG_INFINITY } else { f64::INFINITY }
                        }
                        Special::Finite => pd.d.to_string().parse::<f64>().unwrap_or(f64::NAN),
                    };
                    Ok(vm.ctx.new_float(f).into())
                }),
                ..PyNumberMethods::NOT_IMPLEMENTED
            };
            &AS_NUMBER
        }

        fn clone_exact(zelf: &Py<Self>, _vm: &VirtualMachine) -> PyRef<Self> {
            zelf.to_owned()
        }
    }

    // SignalDict: mpdecimal's dict-subclass used as the type of a Context's
    // `flags`/`traps` attributes. The dict data is shared with the owning
    // Context (a fresh SignalDict view is produced by the `flags`/`traps`
    // getter); a directly-constructed instance (`SignalDict()`) has no
    // backing dict and every operation raises `ValueError: invalid signal
    // dict` (cpython gh-106263 segfault guard).
    #[pyclass(no_attr, name = "SignalDict", module = "decimal", traverse)]
    #[derive(Debug, PyPayload)]
    struct SignalDict {
        dict: Option<PyDictRef>,
    }

    #[pyclass(with(AsMapping, AsSequence, Iterable, Comparable, Representable, Constructor))]
    impl SignalDict {
        fn live_dict(&self, vm: &VirtualMachine) -> PyResult<PyDictRef> {
            self.dict.clone().ok_or_else(|| {
                vm.new_value_error("invalid signal dict".to_string())
            })
        }

        fn mapping_len(&self, vm: &VirtualMachine) -> PyResult<usize> {
            Ok(self.live_dict(vm)?.__len__())
        }

        fn mapping_getitem(&self, needle: &PyObject, vm: &VirtualMachine) -> PyResult {
            self.live_dict(vm)?.get_item(needle, vm)
        }

        fn mapping_setitem(
            &self,
            needle: &PyObject,
            value: PyObjectRef,
            vm: &VirtualMachine,
        ) -> PyResult<()> {
            let d = self.live_dict(vm)?;
            // Keys must be one of the nine signal exception classes.
            if !signal_types().iter().any(|ty| needle.is(ty.as_object())) {
                return Err(vm.new_key_error(needle.to_owned()));
            }
            d.set_item(needle, value, vm)
        }

        fn mapping_delitem(&self, _needle: &PyObject, vm: &VirtualMachine) -> PyResult<()> {
            // mpdecimal: deleting from a signal dict is always an error.
            self.live_dict(vm)?;
            Err(vm.new_value_error("cannot delete items from a signal dict".to_string()))
        }

        fn mapping_contains(&self, target: &PyObject, vm: &VirtualMachine) -> PyResult<bool> {
            Ok(self.live_dict(vm)?.contains_key(target, vm))
        }

        #[pymethod]
        fn get(
            zelf: PyRef<Self>,
            key: PyObjectRef,
            default: OptionalArg<PyObjectRef>,
            vm: &VirtualMachine,
        ) -> PyResult {
            let d = zelf.live_dict(vm)?;
            let args = if let Some(def) = default.into_option() {
                (key, def)
            } else {
                (key, vm.ctx.none())
            };
            vm.call_method(d.as_object(), "get", args)
        }

        #[pymethod]
        fn copy(&self, vm: &VirtualMachine) -> PyResult {
            let d = self.live_dict(vm)?;
            vm.call_method(d.as_object(), "copy", ())
        }

        #[pymethod]
        fn keys(&self, vm: &VirtualMachine) -> PyResult {
            let d = self.live_dict(vm)?;
            vm.call_method(d.as_object(), "keys", ())
        }

        #[pymethod]
        fn values(&self, vm: &VirtualMachine) -> PyResult {
            let d = self.live_dict(vm)?;
            vm.call_method(d.as_object(), "values", ())
        }

        #[pymethod]
        fn items(&self, vm: &VirtualMachine) -> PyResult {
            let d = self.live_dict(vm)?;
            vm.call_method(d.as_object(), "items", ())
        }
    }

    impl AsMapping for SignalDict {
        fn as_mapping() -> &'static PyMappingMethods {
            static AS_MAPPING: PyMappingMethods = PyMappingMethods {
                length: atomic_func!(|mapping, _vm| {
                    SignalDict::mapping_downcast(mapping).mapping_len(_vm)
                }),
                subscript: atomic_func!(|mapping, needle, vm| {
                    SignalDict::mapping_downcast(mapping).mapping_getitem(needle, vm)
                }),
                ass_subscript: atomic_func!(|mapping, needle, value, vm| {
                    let zelf = SignalDict::mapping_downcast(mapping);
                    if let Some(value) = value {
                        zelf.mapping_setitem(needle, value, vm)
                    } else {
                        zelf.mapping_delitem(needle, vm)
                    }
                }),
            };
            &AS_MAPPING
        }
    }

    impl AsSequence for SignalDict {
        fn as_sequence() -> &'static PySequenceMethods {
            static AS_SEQUENCE: LazyLock<PySequenceMethods> = LazyLock::new(|| PySequenceMethods {
                contains: atomic_func!(|seq, target, vm| {
                    SignalDict::sequence_downcast(seq).mapping_contains(target, vm)
                }),
                ..PySequenceMethods::NOT_IMPLEMENTED
            });
            &AS_SEQUENCE
        }
    }

    impl Iterable for SignalDict {
        fn iter(zelf: PyRef<Self>, vm: &VirtualMachine) -> PyResult {
            let d = zelf.live_dict(vm)?;
            vm.call_method(d.as_object(), "__iter__", ())
        }
    }

    impl Comparable for SignalDict {
        fn cmp(
            zelf: &Py<Self>,
            other: &PyObject,
            op: PyComparisonOp,
            vm: &VirtualMachine,
        ) -> PyResult<PyComparisonValue> {
            op.eq_only(|| {
                let self_d = zelf.live_dict(vm)?;
                if let Some(other_sd) = other.downcast_ref::<SignalDict>() {
                    let other_d = other_sd.live_dict(vm)?;
                    let eq = vm.bool_eq(self_d.as_object(), other_d.as_object())?;
                    return Ok(PyComparisonValue::Implemented(eq));
                }
                if let Some(other_d) = other.downcast_ref::<PyDict>() {
                    let eq = vm.bool_eq(self_d.as_object(), other_d.as_object())?;
                    return Ok(PyComparisonValue::Implemented(eq));
                }
                Ok(PyComparisonValue::NotImplemented)
            })
        }
    }

    impl Representable for SignalDict {
        fn repr(zelf: &Py<Self>, vm: &VirtualMachine) -> PyResult<PyRef<PyStr>> {
            let d = zelf.live_dict(vm)?;
            let r = vm.call_method(d.as_object(), "__repr__", ())?;
            r.downcast::<PyStr>().map_err(|o| {
                vm.new_type_error(format!("unexpected repr result: {}", o.class()))
            })
        }
    }

    impl Constructor for SignalDict {
        type Args = ();

        fn py_new(_cls: &Py<PyType>, _args: (), vm: &VirtualMachine) -> PyResult<Self> {
            // Direct construction yields an uninitialized SignalDict whose
            // backing dict is None.
            let _ = vm;
            Ok(SignalDict { dict: None })
        }
    }

    #[pyattr(name = "Context")]
    #[pyclass(name = "Context", module = "decimal", traverse)]
    #[derive(Debug, PyPayload)]
    struct PyContext {
        #[pytraverse(skip)]
        prec: PyMutex<isize>,
        #[pytraverse(skip)]
        rounding: PyMutex<Rounding>,
        #[pytraverse(skip)]
        emin: PyMutex<isize>,
        #[pytraverse(skip)]
        emax: PyMutex<isize>,
        #[pytraverse(skip)]
        capitals: PyMutex<isize>,
        #[pytraverse(skip)]
        clamp: PyMutex<isize>,
        traps: PyMutex<PyDictRef>,
        flags: PyMutex<PyDictRef>,
    }

    impl PyContext {
        fn new_ctx(vm: &VirtualMachine, prec: isize) -> Self {
            let flags = vm.ctx.new_dict();
            let traps = vm.ctx.new_dict();
            for ty in signal_types() {
                let _ = flags.set_item(ty.as_object(), vm.ctx.new_bool(false).into(), vm);
                // CPython DefaultContext: only InvalidOperation, DivisionByZero,
                // and Overflow are trapped by default.
                let trap_on = ty.is(&exc().invalid_operation)
                    || ty.is(&exc().division_by_zero)
                    || ty.is(&exc().overflow);
                let _ = traps.set_item(ty.as_object(), vm.ctx.new_bool(trap_on).into(), vm);
            }
            Self {
                prec: PyMutex::new(prec),
                rounding: PyMutex::new(Rounding::HalfEven),
                emin: PyMutex::new(-999999),
                emax: PyMutex::new(999999),
                capitals: PyMutex::new(1),
                clamp: PyMutex::new(0),
                traps: PyMutex::new(traps),
                flags: PyMutex::new(flags),
            }
        }

        fn ctx(&self) -> Ctx {
            Ctx::from_parts(
                *self.prec.lock(),
                *self.rounding.lock(),
                *self.emax.lock(),
                *self.emin.lock(),
                *self.clamp.lock(),
                self.flags.lock().clone(),
                self.traps.lock().clone(),
            )
        }
    }

    #[pyclass(with(Constructor, Representable))]
    impl PyContext {
        #[pygetset]
        fn prec(&self) -> isize {
            *self.prec.lock()
        }
        #[pygetset(setter)]
        fn set_prec(&self, value: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
            let v: isize = value
                .downcast_ref::<PyInt>()
                .ok_or_else(|| vm.new_type_error("prec must be an integer".to_string()))?
                .try_to_primitive(vm)?;
            if v < 1 {
                return Err(vm.new_value_error("prec must be >= 1".to_string()));
            }
            if v > MAX_PREC {
                return Err(vm.new_value_error("prec must be <= MAX_PREC".to_string()));
            }
            *self.prec.lock() = v;
            Ok(())
        }
        #[pygetset]
        fn rounding(&self) -> String {
            self.rounding.lock().to_str().to_string()
        }
        #[pygetset(setter)]
        fn set_rounding(&self, value: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
            let s = value.downcast_ref::<PyStr>()
                .ok_or_else(|| vm.new_type_error("rounding must be a string".to_string()))?;
            let r = Rounding::from_str(&s.as_wtf8().to_string())
                .ok_or_else(|| vm.new_type_error("invalid rounding mode".to_string()))?;
            *self.rounding.lock() = r;
            Ok(())
        }
        #[pygetset]
        fn Emin(&self) -> isize {
            *self.emin.lock()
        }
        #[pygetset(setter)]
        fn set_Emin(&self, value: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
            let v: isize = value
                .downcast_ref::<PyInt>()
                .ok_or_else(|| vm.new_type_error("Emin must be an integer".to_string()))?
                .try_to_primitive(vm)?;
            if v > 0 {
                return Err(vm.new_value_error("Emin must be <= 0".to_string()));
            }
            if v < MIN_EMIN {
                return Err(vm.new_value_error("Emin must be >= MIN_EMIN".to_string()));
            }
            *self.emin.lock() = v;
            Ok(())
        }
        #[pygetset]
        fn Emax(&self) -> isize {
            *self.emax.lock()
        }
        #[pygetset(setter)]
        fn set_Emax(&self, value: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
            let v: isize = value
                .downcast_ref::<PyInt>()
                .ok_or_else(|| vm.new_type_error("Emax must be an integer".to_string()))?
                .try_to_primitive(vm)?;
            if v < 0 {
                return Err(vm.new_value_error("Emax must be >= 0".to_string()));
            }
            if v > MAX_EMAX {
                return Err(vm.new_value_error("Emax must be <= MAX_EMAX".to_string()));
            }
            *self.emax.lock() = v;
            Ok(())
        }
        #[pygetset]
        fn capitals(&self) -> isize {
            *self.capitals.lock()
        }
        #[pygetset(setter)]
        fn set_capitals(&self, value: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
            let v: isize = value
                .downcast_ref::<PyInt>()
                .ok_or_else(|| vm.new_type_error("capitals must be an integer".to_string()))?
                .try_to_primitive(vm)?;
            if v != 0 && v != 1 {
                return Err(vm.new_value_error("capitals must be 0 or 1".to_string()));
            }
            *self.capitals.lock() = v;
            Ok(())
        }
        #[pygetset]
        fn clamp(&self) -> isize {
            *self.clamp.lock()
        }
        #[pygetset(setter)]
        fn set_clamp(&self, value: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
            let v: isize = value
                .downcast_ref::<PyInt>()
                .ok_or_else(|| vm.new_type_error("clamp must be an integer".to_string()))?
                .try_to_primitive(vm)?;
            if v != 0 && v != 1 {
                return Err(vm.new_value_error("clamp must be 0 or 1".to_string()));
            }
            *self.clamp.lock() = v;
            Ok(())
        }
        #[pygetset]
        fn traps(&self, vm: &VirtualMachine) -> PyObjectRef {
            // Return a live SignalDict view sharing this Context's traps dict.
            SignalDict { dict: Some(self.traps.lock().clone()) }.into_ref(&vm.ctx).into()
        }
        #[pygetset(setter)]
        fn set_traps(&self, value: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
            let dict_value = signal_dict_value(value, "traps", vm)?;
            // CPython: dict missing known signals -> KeyError; extra non-signal
            // keys -> KeyError.
            for (k, _) in dict_value.items_vec() {
                if !signal_types().iter().any(|ty| k.is(ty.as_object())) {
                    let msg: PyObjectRef = vm.ctx.new_str("invalid key in traps").into();
                    return Err(vm.new_key_error(msg));
                }
            }
            // Every SIGNAL must be present in the dict (by exception class);
            // DecimalException and sub-derivatives are not signals.
            const SIGNALS: &[&str] = &[
                "Clamped", "InvalidOperation", "DivisionByZero", "Inexact",
                "FloatOperation", "Overflow", "Rounded", "Subnormal", "Underflow",
            ];
            for name in SIGNALS {
                let ty = exc().by_name(name).unwrap();
                if dict_value.get_item_opt(ty.as_object(), vm).unwrap_or(None).is_none() {
                    let msg: PyObjectRef = vm.ctx.new_str(format!("{} not in traps", ty.name())).into();
                    return Err(vm.new_key_error(msg));
                }
            }
            *self.traps.lock() = dict_value;
            Ok(())
        }
        #[pygetset]
        fn flags(&self, vm: &VirtualMachine) -> PyObjectRef {
            // Return a live SignalDict view sharing this Context's flags dict.
            SignalDict { dict: Some(self.flags.lock().clone()) }.into_ref(&vm.ctx).into()
        }
        #[pygetset(setter)]
        fn set_flags(&self, value: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
            let dict_value = signal_dict_value(value, "flags", vm)?;
            for (k, _) in dict_value.items_vec() {
                if !signal_types().iter().any(|ty| k.is(ty.as_object())) {
                    let msg: PyObjectRef = vm.ctx.new_str("invalid key in flags").into();
                    return Err(vm.new_key_error(msg));
                }
            }
            // Every SIGNAL must be present (see set_traps).
            const SIGNALS: &[&str] = &[
                "Clamped", "InvalidOperation", "DivisionByZero", "Inexact",
                "FloatOperation", "Overflow", "Rounded", "Subnormal", "Underflow",
            ];
            for name in SIGNALS {
                let ty = exc().by_name(name).unwrap();
                if dict_value.get_item_opt(ty.as_object(), vm).unwrap_or(None).is_none() {
                    let msg: PyObjectRef = vm.ctx.new_str(format!("{} not in flags", ty.name())).into();
                    return Err(vm.new_key_error(msg));
                }
            }
            *self.flags.lock() = dict_value;
            Ok(())
        }

        #[pymethod]
        fn copy(zelf: PyRef<Self>, vm: &VirtualMachine) -> PyContext {
            // CPython Context.copy: new flags + traps dicts (deep-copied values)
            let new_flags = vm.ctx.new_dict();
            for (k, v) in zelf.flags.lock().items_vec() {
                let _ = new_flags.set_item(k.as_object(), v.clone(), vm);
            }
            let new_traps = vm.ctx.new_dict();
            for (k, v) in zelf.traps.lock().items_vec() {
                let _ = new_traps.set_item(k.as_object(), v.clone(), vm);
            }
            PyContext {
                prec: PyMutex::new(*zelf.prec.lock()),
                rounding: PyMutex::new(*zelf.rounding.lock()),
                emin: PyMutex::new(*zelf.emin.lock()),
                emax: PyMutex::new(*zelf.emax.lock()),
                capitals: PyMutex::new(*zelf.capitals.lock()),
                clamp: PyMutex::new(*zelf.clamp.lock()),
                traps: PyMutex::new(new_traps),
                flags: PyMutex::new(new_flags),
            }
        }

        #[pymethod]
        fn clear_flags(&self, vm: &VirtualMachine) {
            let keys: Vec<_> = self.flags.lock().items_vec().into_iter().map(|(k, _)| k).collect();
            for k in keys {
                let _ = self.flags.lock().set_item(k.as_object(), vm.ctx.new_bool(false).into(), vm);
            }
        }

        #[pymethod]
        fn __delattr__(&self, _name: PyStrRef, vm: &VirtualMachine) -> PyResult<()> {
            // CPython: attributes are read-only; deletion -> AttributeError.
            Err(vm.new_attribute_error("context attributes cannot be deleted".to_string()))
        }

        #[pymethod]
        fn quantize(&self, x: PyObjectRef, y: PyObjectRef, rounding: OptionalArg<PyStrRef>, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let mut ctx = self.ctx();
            if let Some(r) = rounding.into_option() {
                ctx.rounding = Rounding::from_str(&r.as_wtf8().to_string())
                    .ok_or_else(|| vm.new_value_error("invalid rounding mode".to_string()))?;
            }
            // Operand special values:
            //  - xd or yd signaling NaN -> InvalidOperation, result NaN
            //  - yd quiet NaN -> NaN (no flag)
            //  - xd Infinity or yd Infinity -> InvalidOperation (no finite
            //    exponent to quantize to).
            if xd.special == Special::SignalingNaN || yd.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "quantize with sNaN"));
                }
                let mut n = if yd.special == Special::SignalingNaN && xd.special != Special::SignalingNaN {
                    yd.clone()
                } else {
                    xd.clone()
                };
                n.special = Special::NaN;
                // Payload truncated to context precision (sNaN1234567890 ->
                // NaN67890 at prec 6).
                let digits = n.coeff_str();
                let keep = (ctx.prec as usize).max(1);
                if digits.len() > keep {
                    n.coeff = digits[digits.len() - keep..]
                        .parse::<BigInt>()
                        .unwrap_or_else(|_| BigInt::from(0));
                }
                return Ok(PyDecimal::to_py(vm, n));
            }
            if yd.special == Special::NaN || xd.special == Special::NaN {
                // Any quiet-NaN operand yields the first operand's NaN
                // (payload/sign from xd; quantize(NaN6, NaN5) -> NaN6).
                let n = if xd.special == Special::NaN || xd.special == Special::SignalingNaN {
                    xd.clone()
                } else {
                    yd.clone()
                };
                let mut n = n;
                n.special = Special::NaN;
                let digits = n.coeff_str();
                let keep = (ctx.prec as usize).max(1);
                if digits.len() > keep {
                    n.coeff = digits[digits.len() - keep..]
                        .parse::<BigInt>()
                        .unwrap_or_else(|_| BigInt::from(0));
                }
                return Ok(PyDecimal::to_py(vm, n));
            }
            if xd.special == Special::Infinity || yd.special == Special::Infinity {
                // Both infinite: result is the first operand's Infinity, no
                // flag. Exactly one infinite operand: InvalidOperation -> NaN.
                if xd.special == Special::Infinity && yd.special == Special::Infinity {
                    return Ok(PyDecimal::to_py(vm, xd.clone()));
                }
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "quantize of Infinity"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let r = _decimal_core::quantize(vm, &ctx, &xd, yd.exp)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn add(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let r = _decimal_core::dec_add(&ctx, vm, &xd, &yd)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn subtract(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let r = _decimal_core::dec_sub(&ctx, vm, &xd, &yd)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn multiply(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let r = _decimal_core::dec_mul(&ctx, vm, &xd, &yd)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn divide(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let r = _decimal_core::dec_div(&ctx, vm, &xd, &yd)?;
            // The division's guard digits can be zero for a non-terminating
            // quotient (divx287: 0.1 / 9999e-999999997 -> ... Inexact, but the
            // 10th-11th guard digits are 00 -> apply_context saw no inexactness).
            // Check mathematical inexactness via result * divisor != dividend.
            // The exponent difference is always ~shift (bounded by ~prec+5);
            // for subnormal-clamped results the gap is huge, so skip those
            // (apply_context already flagged them).
            if r.special == Special::Finite
                && yd.special == Special::Finite
                && yd.coeff != BigInt::from(0)
                && xd.coeff != BigInt::from(0)
            {
                let prod_coeff = &r.coeff * &yd.coeff;
                let prod_exp = r.exp + yd.exp;
                let a_exp = xd.exp;
                let diff = (a_exp - prod_exp).max(prod_exp - a_exp);
                let inexact = if diff > ctx.prec + 10 {
                    // Huge gap (subnormal clamping) → definitely inexact.
                    true
                } else if prod_exp < a_exp {
                    let d = (a_exp - prod_exp) as u32;
                    prod_coeff != (&xd.coeff * BigInt::from(10).pow(d))
                } else if prod_exp > a_exp {
                    let d = (prod_exp - a_exp) as u32;
                    (&prod_coeff * BigInt::from(10).pow(d)) != xd.coeff
                } else {
                    prod_coeff != xd.coeff
                };
                if inexact {
                    ctx.set_flag(vm, "Inexact")?;
                    if ctx.trap(vm, "Inexact")? {
                        return Err(new_decimal_error(vm, "Inexact", "inexact result"));
                    }
                }
            }
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn compare(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            // A signaling NaN signals InvalidOperation; a quiet NaN does not,
            // but either makes the result a NaN (payload preserved).
            if xd.special == Special::SignalingNaN || yd.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "comparison with sNaN"));
                }
            }
            if xd.special == Special::NaN
                || xd.special == Special::SignalingNaN
                || yd.special == Special::NaN
                || yd.special == Special::SignalingNaN
            {
                // Result NaN payload/sign selection: a signaling NaN wins
                // over a quiet NaN; otherwise any NaN operand's payload and
                // sign are kept (a plain number does not contribute).
                // CPython: compare(NaN85, sNaN83) -> NaN83;
                // compare(NaN, -sNaN89) -> -NaN89; compare(-Inf, NaN) -> NaN.
                let n = if xd.special == Special::SignalingNaN {
                    xd.clone()
                } else if yd.special == Special::SignalingNaN {
                    yd.clone()
                } else if xd.special == Special::NaN {
                    xd.clone()
                } else {
                    yd.clone()
                };
                let mut n = n;
                n.special = Special::NaN;
                return Ok(PyDecimal::to_py(vm, n));
            }
            let r = dec_cmp(&xd, &yd);
            Ok(vm.ctx.new_int(BigInt::from(r)).into())
        }

        #[pymethod]
        fn remainder(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let r = _decimal_core::dec_mod(&ctx, vm, &xd, &yd)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn power(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            // Context.power(a, b, modulo=None); supports positional and
            // keyword forms (a=, b=, modulo=). Unknown keywords (like 'mod')
            // are a TypeError (CPython).
            for key in args.kwargs.keys() {
                let k = String::from_utf8_lossy(key.as_bytes());
                if k != "a" && k != "b" && k != "modulo" {
                    return Err(vm.new_type_error(format!(
                        "'{k}' is an invalid keyword argument for this function"
                    )));
                }
            }
            let mut pos = args.args.iter();
            let mut get = |key: &str| -> Option<PyObjectRef> {
                args.get_optional_kwarg(key).or_else(|| pos.next().cloned())
            };
            let a = get("a")
                .ok_or_else(|| vm.new_type_error("power expected an argument a".to_string()))?;
            let b = get("b")
                .ok_or_else(|| vm.new_type_error("power expected an argument b".to_string()))?;
            let modulo = get("modulo");
            let ad = to_dec_ctx(&a, vm)?;
            let bd = to_dec_ctx(&b, vm)?;
            let ctx = self.ctx();
            if let Some(m) = modulo {
                let md = to_dec_ctx(&m, vm)?;
                let r = _decimal_core::dec_pow_mod(&ctx, vm, &ad, &bd, &md)?;
                return Ok(PyDecimal::to_py(vm, r));
            }
            let r = _decimal_core::dec_pow_int(&ctx, vm, &ad, &bd)?;
            // The non-integer power path (exp(b*ln(a))) is transcendental:
            // the result is always inexact, but when it fills the precision
            // (no padding needed), _decimal_core doesn't signal Inexact.
            // Signal here (user-facing, no VM perf cliff) for non-integer
            // exponents on non-1 bases with finite nonzero results
            // (pwsx027: power 4.0 0.5 -> 2.00000000 Inexact Rounded).
            if r.special == Special::Finite && !r.is_zero() {
                let is_noninteger = bd.exp < 0
                    && (&bd.coeff % BigInt::from(10).pow((-bd.exp) as u32)) != BigInt::from(0);
                let base_is_one = ad.coeff == BigInt::from(1) && ad.exp == 0 && !ad.sign;
                if is_noninteger && !base_is_one {
                    ctx.set_flag(vm, "Inexact")?;
                    if ctx.trap(vm, "Inexact")? {
                        return Err(new_decimal_error(vm, "Inexact", "inexact result"));
                    }
                    ctx.set_flag(vm, "Rounded")?;
                    if ctx.trap(vm, "Rounded")? {
                        return Err(new_decimal_error(vm, "Rounded", "rounded result"));
                    }
                    // IEEE 754: Underflow when subnormal AND inexact.
                    // The inexactness from the transcendental computation
                    // wasn't visible to apply_context (no digits dropped in
                    // rounding), so Underflow wasn't set there. Set it here
                    // if the result is subnormal (pwsx807: power 1E-20 0.5
                    // -> 1.000000000E-10 Underflow Subnormal Inexact Rounded).
                    let adj = r.exp + _decimal_core::coeff_digits_of(&r.coeff) as isize - 1;
                    if adj < ctx.emin {
                        ctx.set_flag(vm, "Underflow")?;
                        if ctx.trap(vm, "Underflow")? {
                            return Err(_decimal_core::ctx_trap_exc(vm, "Underflow", "underflow"));
                        }
                    }
                }
            } else if r.special == Special::Finite && r.is_zero()
                && !ad.is_zero() && ad.special == Special::Finite
            {
                // The non-integer power underflowed to zero in the wide ctx.
                // apply_context saw a zero and returned early (only Clamped).
                // The computation was inexact and subnormal — re-signal the
                // missing flags (powx4013: power 1.1E-1999997 1.1 ->
                // 0E-1000008 Clamped Inexact Rounded Subnormal Underflow).
                let is_noninteger = bd.exp < 0
                    && (&bd.coeff % BigInt::from(10).pow((-bd.exp) as u32)) != BigInt::from(0);
                if is_noninteger {
                    ctx.set_flag(vm, "Subnormal")?;
                    if ctx.trap(vm, "Subnormal")? {
                        return Err(_decimal_core::ctx_trap_exc(vm, "Subnormal", "subnormal"));
                    }
                    ctx.set_flag(vm, "Underflow")?;
                    if ctx.trap(vm, "Underflow")? {
                        return Err(_decimal_core::ctx_trap_exc(vm, "Underflow", "underflow"));
                    }
                    ctx.set_flag(vm, "Inexact")?;
                    if ctx.trap(vm, "Inexact")? {
                        return Err(new_decimal_error(vm, "Inexact", "inexact result"));
                    }
                    ctx.set_flag(vm, "Rounded")?;
                    if ctx.trap(vm, "Rounded")? {
                        return Err(new_decimal_error(vm, "Rounded", "rounded result"));
                    }
                }
            }
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn sqrt(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            // Share the core with Decimal.sqrt; this used to carry a
            // separate f64-seeded Newton loop that ignored the context.
            let r = _decimal_core::dec_sqrt(vm, &ctx, &xd)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn create_decimal(&self, value: OptionalArg<PyObjectRef>, vm: &VirtualMachine) -> PyResult {
            let value = match value.into_option() {
                Some(v) => v,
                None => vm.ctx.new_int(0).into(),
            };
            // CPython: Context.create_decimal does NOT trim whitespace or strip
            // underscores from string args (unlike Decimal()).
            // '3.14\n' / ' 1234' / '12_34' -> InvalidOperation -> NaN.
            if let Some(s) = value.downcast_ref::<PyStr>() {
                let txt = s.as_wtf8().to_string();
                if txt != txt.trim() || txt.contains('_') {
                    let ctx = self.ctx();
                    ctx.set_flag(vm, "InvalidOperation")?;
                    if ctx.trap(vm, "InvalidOperation")? {
                        return Err(new_decimal_error(vm, "InvalidOperation", "conversion syntax"));
                    }
                    return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                        sign: false, coeff: BigInt::from(0), exp: 0, special: _decimal_core::Special::NaN,
                    }));
                }
            }
            // CPython: Context.create_decimal uses string parsing (to_dec),
            // but strings with internal whitespace -> NaN (InvalidOperation).
            // TypeError (e.g. None) must propagate.
            let d = match to_dec(&value, vm) {
                Ok(d) => d,
                Err(e) => {
                    // Only InvalidOperation (string parse failure) -> NaN;
                    // TypeError/ValueError propagate.
                    if e.class().is(&exc().invalid_operation) {
                        let ctx = self.ctx();
                        ctx.set_flag(vm, "InvalidOperation")?;
                        if ctx.trap(vm, "InvalidOperation")? {
                            return Err(e);
                        }
                        _decimal_core::Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: _decimal_core::Special::NaN }
                    } else {
                        return Err(e);
                    }
                }
            };
            let mut ctx = self.ctx();
            // CPython: NaN payload digits must not exceed context precision.
            if d.is_special() && d.special == _decimal_core::Special::NaN
                || d.special == _decimal_core::Special::SignalingNaN
            {
                let payload_digits = _decimal_core::coeff_digits_of(&d.coeff) as isize;
                if payload_digits > ctx.prec {
                    ctx.set_flag(vm, "InvalidOperation")?;
                    if ctx.trap(vm, "InvalidOperation")? {
                        return Err(new_decimal_error(vm, "InvalidOperation", "NaN payload too long"));
                    }
                    return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                        sign: d.sign, coeff: BigInt::from(0), exp: 0, special: _decimal_core::Special::NaN,
                    }));
                }
            }
            if value.downcast_ref::<PyFloat>().is_some() {
                ctx.set_flag(vm, "FloatOperation")?;
                if ctx.trap(vm, "FloatOperation")? {
                    return Err(new_decimal_error(
                        vm,
                        "FloatOperation",
                        "float conversion is not safe for Decimal",
                    ));
                }
            }
            let r = _decimal_core::apply_context(vm, &ctx, d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn create_decimal_from_float(&self, value: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let d = if let Some(f) = value.downcast_ref::<PyFloat>() {
                _decimal_core::dec_from_float(f.to_f64())
            } else if let Some(i) = value.downcast_ref::<PyInt>() {
                _decimal_core::dec_from_int(vm, i)
            } else {
                return Err(vm.new_type_error(
                    "argument must be an int or float".to_string(),
                ));
            };
            let ctx = self.ctx();
            let r = _decimal_core::apply_context(vm, &ctx, d)?;
            Ok(PyDecimal::to_py(vm, r))
        }


        #[pymethod]
        fn __str__(&self, vm: &VirtualMachine) -> PyResult<String> {
            Ok(ctx_repr_string_impl(self, vm))
        }

        #[pymethod]
        fn __reduce__(zelf: PyRef<Self>, vm: &VirtualMachine) -> PyResult {
            // CPython Context.__reduce__ returns
            // (Context, (prec, rounding, Emin, Emax, capitals, clamp,
            //  [trap names], flag_names_and_values))
            let prec = *zelf.prec.lock();
            let rounding = *zelf.rounding.lock();
            let rounding_str = rounding.to_str().to_string();
            let emin = *zelf.emin.lock();
            let emax = *zelf.emax.lock();
            let capitals = *zelf.capitals.lock();
            let clamp = *zelf.clamp.lock();
            // traps/Flags: list of exception objects that are True (matching
            // CPython's __reduce__ which passes Python lists).
            let trap_names: Vec<PyObjectRef> = zelf
                .traps
                .lock()
                .items_vec()
                .into_iter()
                .filter(|(_, v)| v.try_to_value::<bool>(vm).unwrap_or(false))
                .map(|(k, _)| k)
                .collect();
            let flag_names: Vec<PyObjectRef> = zelf
                .flags
                .lock()
                .items_vec()
                .into_iter()
                .filter(|(_, v)| v.try_to_value::<bool>(vm).unwrap_or(false))
                .map(|(k, _)| k)
                .collect();
            let trap_list = vm.ctx.new_list(trap_names);
            let flag_list = vm.ctx.new_list(flag_names);
            // CPython order: (prec, rounding, Emin, Emax, capitals, clamp,
            // flags, traps) — flags is the 7th positional arg, traps the 8th.
            let args: PyObjectRef = vm.ctx.new_tuple(vec![
                vm.ctx.new_int(prec).into(),
                vm.ctx.new_str(rounding_str).into(),
                vm.ctx.new_int(emin).into(),
                vm.ctx.new_int(emax).into(),
                vm.ctx.new_int(capitals).into(),
                vm.ctx.new_int(clamp).into(),
                flag_list.into(),
                trap_list.into(),
            ]).into();
            let cls: PyObjectRef = zelf.class().to_owned().into();
            Ok(vm.ctx.new_tuple(vec![cls, args]).into())
        }

        // --- Missing Context methods (delegating wrappers) ---
        #[pymethod]
        fn abs(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            // CPython: a signaling NaN operand raises InvalidOperation; the
            // result is a quiet NaN (payload and sign preserved).
            if xd.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "abs with sNaN"));
                }
                let mut n = xd.clone();
                n.special = Special::NaN;
                return Ok(PyDecimal::to_py(vm, n));
            }
            // NaN keeps its sign (abs(-NaN22) == -NaN22); Infinity loses it.
            if xd.special == Special::NaN {
                return Ok(PyDecimal::to_py(vm, xd.clone()));
            }
            let mut d = xd.clone();
            d.sign = false;
            let r = _decimal_core::apply_context(vm, &ctx, d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn minus(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            if xd.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "minus with sNaN"));
                }
                let mut n = xd.clone();
                n.special = Special::NaN;
                return Ok(PyDecimal::to_py(vm, n));
            }
            // -(-0) = +0 (zero loses the sign); a NaN keeps its sign
            // (minus(-NaN22) == -NaN22).
            let mut d = xd.clone();
            if d.is_zero() {
                d.sign = false;
            } else if d.special != Special::NaN {
                d.sign = !d.sign;
            }
            let r = _decimal_core::apply_context(vm, &ctx, d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn plus(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            if xd.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "plus with sNaN"));
                }
                let mut n = xd.clone();
                n.special = Special::NaN;
                return Ok(PyDecimal::to_py(vm, n));
            }
            // Unary plus clears a negative zero (plus(-0E2) -> 0E+2) but
            // keeps a NaN's sign.
            let mut d = xd.clone();
            if d.is_zero() {
                d.sign = false;
            }
            let r = _decimal_core::apply_context(vm, &ctx, d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn copy_abs(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let mut d = to_dec_ctx(&x, vm)?;
            d.sign = false;
            Ok(PyDecimal::to_py(vm, d))
        }

        #[pymethod]
        fn copy_negate(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let mut d = to_dec_ctx(&x, vm)?;
            d.sign = !d.sign;
            Ok(PyDecimal::to_py(vm, d))
        }

        #[pymethod]
        fn copy_sign(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let mut d = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            d.sign = yd.sign;
            Ok(PyDecimal::to_py(vm, d))
        }

        #[pymethod]
        fn copy_decimal(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            // CPython copy_decimal uses PyDec_FromObjectExact: strings are
            // rejected with TypeError (int/Decimal are accepted).
            let d = to_dec_ctx(&x, vm)?;
            Ok(PyDecimal::to_py(vm, d))
        }

        #[pymethod]
        fn compare_signal(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let is_nan = |d: &Dec| {
                d.special == Special::NaN || d.special == Special::SignalingNaN
            };
            if is_nan(&xd) || is_nan(&yd) {
                // compare_signal: any NaN operand signals InvalidOperation and,
                // when not trapped, yields a quiet NaN (not a numeric order).
                // The sign/payload follow the first NaN operand (dqcms840:
                // compare_signal(-NaN, +NaN) -> -NaN; ddcms824: (NaN, -0) ->
                // NaN).
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "comparison with NaN"));
                }
                let src = if is_nan(&xd) { &xd } else { &yd };
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: src.sign,
                    coeff: src.coeff.clone(),
                    exp: 0,
                    special: Special::NaN,
                }));
            }
            let r = dec_cmp(&xd, &yd);
            Ok(vm.ctx.new_int(BigInt::from(r)).into())
        }

        #[pymethod]
        fn compare_total(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let order = |d: &Dec| -> i64 {
                match d.special {
                    // Total order (CPython):
                    // -NaN < -sNaN < -Inf < 负有限 < -0 < +0 < 正有限 < +Inf
                    // < +sNaN < +NaN.
                    Special::NaN => if d.sign { 0 } else { 10 },
                    Special::SignalingNaN => if d.sign { 1 } else { 9 },
                    Special::Infinity => if d.sign { 2 } else { 8 },
                    Special::Finite => {
                        if d.is_zero() { if d.sign { 4 } else { 5 } }
                        else { if d.sign { 3 } else { 6 } }
                    }
                }
            };
            let ox = order(&xd);
            let oy = order(&yd);
            let r = if ox != oy {
                match ox.cmp(&oy) {
                    core::cmp::Ordering::Less => -1,
                    core::cmp::Ordering::Equal => 0,
                    core::cmp::Ordering::Greater => 1,
                }
            } else if xd.exp != yd.exp {
                // Same class: compare numerically first; only equal values
                // fall through to the exponent ordering. A larger exponent
                // sorts first for positives, last for negatives
                // (comparetotal(12, 12.00) -> 1; comparetotal(-0, -0.0)
                // -> -1).
                let c = dec_cmp(&xd, &yd);
                if c != 0 {
                    c
                } else {
                    let bigger = if xd.exp > yd.exp { 1 } else { -1 };
                    if xd.sign { -bigger } else { bigger }
                }
            } else {
                let is_nan = |d: &Dec| {
                    d.special == Special::NaN || d.special == Special::SignalingNaN
                };
                // Same NaN class: order by the diagnostic payload
                // (comparetotal(NaN42, NaN01) -> 1).
                if is_nan(&xd) && is_nan(&yd) {
                    nan_payload_cmp(&xd, &yd)
                } else {
                    dec_cmp(&xd, &yd)
                }
            };
            Ok(vm.ctx.new_int(BigInt::from(r)).into())
        }

        #[pymethod]
        fn compare_total_mag(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let mut xd = to_dec_ctx(&x, vm)?;
            let mut yd = to_dec_ctx(&y, vm)?;
            xd.sign = false;
            yd.sign = false;
            let order = |d: &Dec| -> i64 {
                match d.special {
                    // Magnitude total order (signs cleared): zero < finite <
                    // +Inf < +sNaN < +NaN.
                    Special::NaN => 10,
                    Special::SignalingNaN => 9,
                    Special::Infinity => 8,
                    Special::Finite => {
                        if d.is_zero() { 5 } else { 6 }
                    }
                }
            };
            let ox = order(&xd);
            let oy = order(&yd);
            let r = if ox != oy {
                match ox.cmp(&oy) {
                    core::cmp::Ordering::Less => -1,
                    core::cmp::Ordering::Equal => 0,
                    core::cmp::Ordering::Greater => 1,
                }
            } else if xd.exp != yd.exp {
                let c = dec_cmp(&xd, &yd);
                if c != 0 {
                    c
                } else if xd.exp > yd.exp {
                    1
                } else {
                    -1
                }
            } else {
                let is_nan = |d: &Dec| {
                    d.special == Special::NaN || d.special == Special::SignalingNaN
                };
                // Signs are cleared above: same NaN class orders by payload
                // (comparetotmag(-NaN42, NaN01) -> 1; -sNaN771, sNaN772 -> -1).
                if is_nan(&xd) && is_nan(&yd) {
                    nan_payload_cmp(&xd, &yd)
                } else {
                    dec_cmp(&xd, &yd)
                }
            };
            Ok(vm.ctx.new_int(BigInt::from(r)).into())
        }

        #[pymethod]
        fn divide_int(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            // CPython divideint/floordiv: exact floor division (no Rounded/
            // Inexact from a true division: 1 // 7 -> 0 with no flags).
            let r = _decimal_core::dec_floordiv(&ctx, vm, &xd, &yd)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn divmod(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let nan = _decimal_core::Dec {
                sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
            };
            let tuple = |a: _decimal_core::Dec, b: _decimal_core::Dec| {
                Ok(vm.ctx.new_tuple(vec![PyDecimal::to_py(vm, a), PyDecimal::to_py(vm, b)]).into())
            };
            let is_nan = |d: &Dec| d.special == Special::NaN || d.special == Special::SignalingNaN;
            if is_nan(&xd) || is_nan(&yd) {
                // A NaN operand yields (NaN, NaN); only an sNaN signals.
                if xd.special == Special::SignalingNaN || yd.special == Special::SignalingNaN {
                    ctx.set_flag(vm, "InvalidOperation")?;
                    if ctx.trap(vm, "InvalidOperation")? {
                        return Err(new_decimal_error(vm, "InvalidOperation", "sNaN operand"));
                    }
                }
                return tuple(nan.clone(), nan);
            }
            if xd.is_special() || yd.is_special() {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "invalid operation"));
                }
                return tuple(nan.clone(), nan);
            }
            if yd.is_zero() {
                // InvalidOperation takes precedence over DivisionByZero.
                ctx.set_flag(vm, "InvalidOperation")?;
                ctx.set_flag(vm, "DivisionByZero")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "invalid operation"));
                }
                if ctx.trap(vm, "DivisionByZero")? {
                    return Err(new_decimal_error(vm, "DivisionByZero", "division by zero"));
                }
                let inf = _decimal_core::Dec {
                    sign: xd.sign,
                    coeff: BigInt::from(0),
                    exp: 0,
                    special: Special::Infinity,
                };
                return tuple(inf, nan);
            }
            // CPython's divmod truncates the quotient toward zero (unlike //,
            // which floors), then takes the remainder against that quotient.
            let quotient = _decimal_core::dec_div(&ctx, vm, &xd, &yd)?;
            let q = truncate_toward_zero(&quotient);
            // The integral part must fit the context, else InvalidOperation:
            // divmod(1E+10, 1) at prec 5 raises rather than returning 1E+10.
            // Digit count of the VALUE, so 1E+10 counts as eleven digits
            // even though its coefficient is a single 1.
            let mut q_digits = _decimal_core::coeff_digits_of(&q.coeff);
            if q.exp > 0 {
                q_digits += q.exp as usize;
            }
            if !q.is_zero() && q_digits > ctx.prec as usize {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "invalid operation"));
                }
                return tuple(nan.clone(), nan);
            }
            let product = _decimal_core::dec_mul(&ctx, vm, &q, &yd)?;
            let mut remainder = _decimal_core::dec_sub(&ctx, vm, &xd, &product)?;
            let mut q = if q.is_zero() {
                _decimal_core::Dec {
                    sign: xd.sign ^ yd.sign,
                    coeff: BigInt::from(0),
                    exp: 0,
                    special: Special::Finite,
                }
            } else {
                q
            };
            if remainder.is_zero() {
                // A zero remainder takes the sign of x alone, whatever the
                // divisor: divmod(-1, 1) -> (-1, -0) but divmod(2, -2) -> (-1, 0).
                // It also keeps the divisor exponent, so divmod(0, 0.5) -> (0, 0.0).
                remainder.sign = xd.sign;
                // A zero remainder takes min(dividend exp, divisor exp):
                // divmod(1.00, 0.5) -> (2, 0.00) wants -2, divmod(1, 0.5) ->
                // (2, 0.0) wants -1, and divmod(1E+10, 1) -> (10000000000, 0)
                // wants 0. No single operand explains all three; the minimum
                // does.
                remainder.exp = xd.exp.min(yd.exp);
            }
            if !q.is_zero() {
                // The integral quotient is written with exponent 0, the same rule
                // log10 uses: divmod(1E+10, 1) is 10000000000, not 1E+10.
                let ten = BigInt::from(10);
                while (&q.coeff % &ten) == BigInt::from(0) {
                    q.coeff = &q.coeff / &ten;
                    q.exp += 1;
                }
                if q.exp > 0 {
                    q.coeff *= BigInt::from(10).pow(q.exp as u32);
                    q.exp = 0;
                }
            }
            let q = _decimal_core::apply_context(vm, &ctx, q)?;
            let remainder = _decimal_core::apply_context(vm, &ctx, remainder)?;
            tuple(q, remainder)
        }
        #[pymethod]
        fn logb(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            if xd.is_special() {
                if xd.special == Special::Infinity {
                    // logb(Infinity) -> +Infinity (unsigned): the adjusted
                    // exponent of any infinite value is +Infinity.
                    return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                        sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Infinity,
                    }));
                }
                // NaN / sNaN: signal InvalidOperation on sNaN and return a
                // quiet NaN (payload and sign preserved); a quiet NaN
                // operand propagates without signaling.
                if xd.special == Special::SignalingNaN {
                    ctx.set_flag(vm, "InvalidOperation")?;
                    if ctx.trap(vm, "InvalidOperation")? {
                        return Err(new_decimal_error(vm, "InvalidOperation", "logb of sNaN"));
                    }
                }
                let mut n = xd.clone();
                n.special = Special::NaN;
                let digits = n.coeff_str();
                let keep = (ctx.prec as usize).max(1);
                if digits.len() > keep {
                    n.coeff = digits[digits.len() - keep..]
                        .parse::<BigInt>()
                        .unwrap_or_else(|_| BigInt::from(0));
                }
                return Ok(PyDecimal::to_py(vm, n));
            }
            if xd.is_zero() {
                ctx.set_flag(vm, "DivisionByZero")?;
                if ctx.trap(vm, "DivisionByZero")? {
                    return Err(new_decimal_error(vm, "DivisionByZero", "logb of zero"));
                }
                // logb(0) -> -Infinity regardless of the zero's sign.
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: true, coeff: BigInt::from(0), exp: 0, special: Special::Infinity,
                }));
            }
            let adj = xd.exp + _decimal_core::coeff_digits_of(&xd.coeff) as isize - 1;
            // Result is the adjusted exponent as a decimal integer; it is
            // rounded to the context precision (logb(1E-999) at prec 2 ->
            // -1.0E+3, i.e. 999 rounded to 1000).
            let r = _decimal_core::apply_context(vm, &ctx, _decimal_core::Dec {
                sign: adj < 0, coeff: BigInt::from(adj.unsigned_abs()), exp: 0,
                special: Special::Finite,
            })?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn max(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            // NaN handling: a signaling NaN always signals InvalidOperation
            // and yields a quiet NaN (payload kept) even when the other
            // operand is a number (min(sNaN99,-Inf) -> NaN99); a quiet NaN
            // is ignored when the other operand is a number (max(NaN,1000)
            // -> 1000); both quiet NaN -> NaN.
            let x_snan = xd.special == Special::SignalingNaN;
            let y_snan = yd.special == Special::SignalingNaN;
            let x_qnan = xd.special == Special::NaN;
            let y_qnan = yd.special == Special::NaN;
            if x_snan || y_snan {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "max with sNaN"));
                }
                let mut n = if x_snan {
                    xd.clone()
                } else {
                    yd.clone()
                };
                n.special = Special::NaN;
                return Ok(PyDecimal::to_py(vm, n));
            }
            if x_qnan || y_qnan {
                let n = if x_qnan && y_qnan {
                    let mut n = xd.clone();
                    n.special = Special::NaN;
                    n
                } else if x_qnan {
                    yd.clone()
                } else {
                    xd.clone()
                };
                return Ok(PyDecimal::to_py(vm, n));
            }
            let r = if max_picks_a(&xd, &yd) { xd } else { yd };
            let r = _decimal_core::apply_context(vm, &ctx, r)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn max_mag(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            // NaN handling mirrors max (sNaN signals; quiet NaN ignored).
            let x_snan = xd.special == Special::SignalingNaN;
            let y_snan = yd.special == Special::SignalingNaN;
            let x_qnan = xd.special == Special::NaN;
            let y_qnan = yd.special == Special::NaN;
            if x_snan || y_snan {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "max_mag with sNaN"));
                }
                let mut n = if x_snan {
                    xd.clone()
                } else {
                    yd.clone()
                };
                n.special = Special::NaN;
                return Ok(PyDecimal::to_py(vm, n));
            }
            if x_qnan || y_qnan {
                let n = if x_qnan && y_qnan {
                    let mut n = xd.clone();
                    n.special = Special::NaN;
                    n
                } else if x_qnan {
                    yd.clone()
                } else {
                    xd.clone()
                };
                return Ok(PyDecimal::to_py(vm, n));
            }
            let r = if max_mag_picks_a(&xd, &yd) { xd } else { yd };
            let r = _decimal_core::apply_context(vm, &ctx, r)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn min(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            // NaN handling mirrors max: sNaN signals InvalidOperation and yields a
            // quiet NaN (payload kept) even with a numeric operand; a quiet
            // NaN is ignored when the other operand is a number; both quiet
            // NaN -> NaN.
            let x_snan = xd.special == Special::SignalingNaN;
            let y_snan = yd.special == Special::SignalingNaN;
            let x_qnan = xd.special == Special::NaN;
            let y_qnan = yd.special == Special::NaN;
            if x_snan || y_snan {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "min with sNaN"));
                }
                let mut n = if x_snan {
                    xd.clone()
                } else {
                    yd.clone()
                };
                n.special = Special::NaN;
                return Ok(PyDecimal::to_py(vm, n));
            }
            if x_qnan || y_qnan {
                let n = if x_qnan && y_qnan {
                    let mut n = xd.clone();
                    n.special = Special::NaN;
                    n
                } else if x_qnan {
                    yd.clone()
                } else {
                    xd.clone()
                };
                return Ok(PyDecimal::to_py(vm, n));
            }
            let r = if min_picks_a(&xd, &yd) { xd } else { yd };
            let r = _decimal_core::apply_context(vm, &ctx, r)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn min_mag(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            // NaN handling mirrors min (sNaN signals; quiet NaN ignored).
            let x_snan = xd.special == Special::SignalingNaN;
            let y_snan = yd.special == Special::SignalingNaN;
            let x_qnan = xd.special == Special::NaN;
            let y_qnan = yd.special == Special::NaN;
            if x_snan || y_snan {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "min_mag with sNaN"));
                }
                let mut n = if x_snan {
                    xd.clone()
                } else {
                    yd.clone()
                };
                n.special = Special::NaN;
                return Ok(PyDecimal::to_py(vm, n));
            }
            if x_qnan || y_qnan {
                let n = if x_qnan && y_qnan {
                    let mut n = xd.clone();
                    n.special = Special::NaN;
                    n
                } else if x_qnan {
                    yd.clone()
                } else {
                    xd.clone()
                };
                return Ok(PyDecimal::to_py(vm, n));
            }
            let r = if min_mag_picks_a(&xd, &yd) { xd } else { yd };
            let r = _decimal_core::apply_context(vm, &ctx, r)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn remainder_near(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            // A NaN operand propagates first (decTest rmnx: a quiet NaN gives
            // NaN with no signal; a signaling NaN signals Invalid_operation
            // with its payload quieted).
            if xd.special == Special::SignalingNaN || yd.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "Division_undefined"));
                }
                let src = if xd.special == Special::SignalingNaN { &xd } else { &yd };
                return Ok(PyDecimal::to_py(vm, quiet_snan_payload(&ctx, src)));
            }
            if xd.special == Special::NaN {
                return Ok(PyDecimal::to_py(vm, xd.clone()));
            }
            if yd.special == Special::NaN {
                return Ok(PyDecimal::to_py(vm, yd.clone()));
            }
            // A ZERO divisor is always InvalidOperation + NaN for remainder-near
            // (decTest rmnx101-120: 0 % 0 -> NaN Division_undefined and
            // 1 % 0 -> NaN Invalid_operation - never Division_by_zero's
            // Infinity, and checked before Division_impossible so rmnx113's
            // 1E+100 % 0.0 is Invalid_operation).
            if yd.special == Special::Finite && yd.coeff == BigInt::from(0) {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "Division_undefined"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            // An INFINITE divisor: x / Infinity = 0, so n = 0 and the result
            // is x itself for a FINITE x (rmnx688: remaindernear -1000 Inf
            // -> -1000; no Inf*0 to trigger 0*Inf's Invalid_operation). An
            // INFINITE x with an infinite divisor is Inf/Inf -> Invalid
            // (rmnx680: remaindernear Inf -Inf -> NaN Invalid_operation).
            if yd.special == Special::Infinity {
                if xd.special == Special::Infinity {
                    ctx.set_flag(vm, "InvalidOperation")?;
                    if ctx.trap(vm, "InvalidOperation")? {
                        return Err(new_decimal_error(vm, "InvalidOperation", "Inf % Inf"));
                    }
                    return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                        sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                    }));
                }
                let r = _decimal_core::apply_context(vm, &ctx, xd.clone())?;
                return Ok(PyDecimal::to_py(vm, r));
            }
            // Division impossible: an integer quotient far above the context
            // precision cannot be computed (and building 10**exp below would
            // be unbounded), decTest rmnx081-083.
            let adj = |d: &_decimal_core::Dec| {
                d.exp + _decimal_core::coeff_digits_of(&d.coeff) as isize - 1
            };
            if xd.special == Special::Infinity
                || (xd.special == Special::Finite
                    && yd.special == Special::Finite
                    && adj(&xd) - adj(&yd) >= ctx.prec + 1)
            {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "DivisionImpossible"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            // The internal division/multiply/subtract run in an ISOLATED wide
            // context: the intermediate quotient of remaindernear(1, 3) is
            // inexact, and running it at the caller's precision leaked
            // Inexact+Rounded into the caller's flags (decTest rmnx007
            // expects none - the final remainder 1 is exact).
            let wide = Ctx::from_parts(50, Rounding::HalfEven, 999999999, -999999999, 0,
                vm.ctx.new_dict(), vm.ctx.new_dict());
            let div = _decimal_core::dec_div(&wide, vm, &xd, &yd)?;
            let mut coeff = div.coeff.clone();
            if div.exp < 0 {
                // Fast path for an ultra-tiny quotient (rmnx270: the shift
                // exceeds the coefficient's digit count, the integer part is
                // 0 and no tie is possible - avoid 10**shift for huge
                // exponents). See the instance variant for the full note.
                let shift = (-div.exp) as usize;
                let digits = _decimal_core::coeff_digits_of(&coeff) as usize;
                if shift > digits {
                    coeff = BigInt::from(0);
                } else {
                    let pow = BigInt::from(10).pow(shift as u32);
                    let (q, r) = (&coeff / &pow, &coeff % &pow);
                    let round_up = if &r * BigInt::from(2) > pow { true }
                        else if &r * BigInt::from(2) < pow { false }
                        else { (&q % BigInt::from(2)) == BigInt::from(1) };
                    coeff = if round_up { q + BigInt::from(1) } else { q };
                }
            } else if div.exp > 0 {
                coeff *= BigInt::from(10).pow(div.exp as u32);
            }
            // After rounding to the nearest integer the quotient must still fit
            // the context precision: decTest rmnx076-080 - 999999999 % 1 is
            // fine (n = 999999999, 9 digits at prec 9) but 999999999.5 % 1
            // rounds to n = 1000000000, 10 digits -> Division_impossible.
            if _decimal_core::coeff_digits_of(&coeff) > ctx.prec as usize {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "DivisionImpossible"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let rounded = _decimal_core::Dec { sign: div.sign, coeff, exp: 0, special: Special::Finite };
            let prod = _decimal_core::dec_mul(&wide, vm, &yd, &rounded)?;
            let mut result = _decimal_core::dec_sub(&wide, vm, &xd, &prod)?;
            // GDA: the zero-sign of remainder_near follows the dividend
            // (rmnx134: remaindernear -0 1 -> -0; remaindernear 0 1 -> 0).
            if result.is_zero() {
                result.sign = xd.sign;
            }
            let r = _decimal_core::apply_context(vm, &ctx, result)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn scaleb(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            // A signaling NaN signals InvalidOperation and yields a quiet NaN
            // with the sNaN operand's payload/sign (scaleb(NaN016, sNaN191)
            // -> NaN191; scaleb(Inf, -sNaN) -> -NaN).
            if xd.special == Special::SignalingNaN || yd.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "scaleb with sNaN"));
                }
                let src = if xd.special == Special::SignalingNaN { &xd } else { &yd };
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: src.sign,
                    coeff: src.coeff.clone(),
                    exp: 0,
                    special: Special::NaN,
                }));
            }
            // A quiet NaN operand (no sNaN) passes through without signaling
            // (scbx021/022: scaleb(NaN,1) -> NaN; scaleb(-NaN,-1) -> -NaN).
            if xd.special == Special::NaN || yd.special == Special::NaN {
                let src = if xd.special == Special::NaN { &xd } else { &yd };
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: src.sign,
                    coeff: src.coeff.clone(),
                    exp: 0,
                    special: Special::NaN,
                }));
            }
            // An infinite exponent operand is InvalidOperation too
            // (scbx018: scaleb(10, Infinity) -> NaN Invalid_operation).
            if yd.special == Special::Infinity {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "scaleb with infinite exponent"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            // The exponent must be an integer in bare form (y.exp == 0):
            // scaleb(1.23, 2.0) and scaleb(1.23, 0E+2) are InvalidOperation.
            if yd.exp != 0 {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "scaleb requires an integer"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let n: isize = yd.coeff.to_string().parse().unwrap_or(isize::MAX);
            let n = if yd.sign { -n } else { n };
            // CPython: liminf/limsup = +/-2*(Emax+prec); out of range is
            // InvalidOperation (scbx127: -2018 at prec 9/Emax 999 is below
            // -2016). Inside the range the result flows through
            // apply_context, so a large positive n overflows to Infinity.
            let limsup = 2isize.checked_mul(ctx.emax + ctx.prec).unwrap_or(isize::MAX);
            let liminf = -2isize.checked_mul(ctx.emax + ctx.prec).unwrap_or(isize::MIN);
            if n < liminf || n > limsup {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "scaleb exponent out of range"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let result = _decimal_core::Dec {
                sign: xd.sign, coeff: xd.coeff.clone(), exp: xd.exp + n,
                special: xd.special,
            };
            let r = _decimal_core::apply_context(vm, &ctx, result)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn shift(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            // A NaN operand propagates before the count is validated (decTest
            // shix821-830: shift(NaN, -Inf) and shift(-Inf, NaN) are plain
            // NaN with NO signal; shix810-815 show that a non-NaN operand
            // with an infinite count is Invalid_operation).
            if xd.special == Special::SignalingNaN || yd.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "shift with sNaN"));
                }
                let src = if xd.special == Special::SignalingNaN { &xd } else { &yd };
                return Ok(PyDecimal::to_py(vm, quiet_snan_payload(&ctx, src)));
            }
            if xd.special == Special::NaN {
                return Ok(PyDecimal::to_py(vm, xd.clone()));
            }
            if yd.special == Special::NaN {
                return Ok(PyDecimal::to_py(vm, yd.clone()));
            }
            // The shift count must be a plain integer; shift(1, 0.1) is
            // InvalidOperation. A negative count is legal (shifts right).
            if yd.exp != 0 || yd.special != Special::Finite {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "shift requires an integer count"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let mut n: isize = yd.coeff.to_string().parse().unwrap_or(0);
            if yd.sign {
                n = -n;
            }
            // CPython's mpd_qshiftn only shifts when the count fits the
            // context: 0 <= n <= prec shifts left, -prec <= n < 0 shifts right,
            // and anything else is InvalidOperation rather than a rounded zero.
            // A count too large for isize cannot be in range either.
            let in_range = yd.coeff.to_string().parse::<isize>().is_ok()
                && -(ctx.prec as isize) <= n
                && n <= ctx.prec as isize;
            if !in_range {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "shift amount out of range"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let coeff = if n >= 0 {
                // The left shift overflows the prec-wide window from the top:
                // keep the LOW prec digits silently (shift(12345, 4) at
                // prec 5 -> 50000), no rounding, no signal.
                let mut c = &xd.coeff * BigInt::from(10).pow(n as u32);
                let cd = _decimal_core::coeff_digits_of(&c);
                if cd > ctx.prec as usize {
                    let window = BigInt::from(10).pow(ctx.prec as u32);
                    c = c % window;
                }
                c
            } else {
                let pow = BigInt::from(10).pow((-n) as u32);
                &xd.coeff / &pow
            };
            let result = _decimal_core::Dec {
                sign: xd.sign, coeff, exp: xd.exp, special: xd.special,
            };
            // decTest shift: no case carries any signal. Return as-is.
            Ok(PyDecimal::to_py(vm, result))
        }

        /// Logical operations are digit-wise over 0/1 coefficients (CPython
        /// `_islogical`), not integer bitwise ops: logical_and(12, 10) is
        /// InvalidOperation, not 8.
        fn logical_binary(
            &self,
            x: &PyObjectRef,
            y: &PyObjectRef,
            vm: &VirtualMachine,
            name: &str,
            op: fn(u32, u32) -> u32,
        ) -> PyResult {
            let ctx = self.ctx();
            let xd = to_dec_ctx(x, vm)?;
            let yd = to_dec_ctx(y, vm)?;
            let r = logical_binary_op(vm, &ctx, &xd, &yd, name, op)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn logical_and(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            self.logical_binary(&x, &y, vm, "logical_and requires non-negative integers", |a, b| a & b)
        }

        #[pymethod]
        fn logical_or(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            self.logical_binary(&x, &y, vm, "logical_or requires non-negative integers", |a, b| a | b)
        }

        #[pymethod]
        fn logical_xor(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            self.logical_binary(&x, &y, vm, "logical_xor requires non-negative integers", |a, b| a ^ b)
        }

        #[pymethod]
        fn logical_invert(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let ctx = self.ctx();
            let xd = to_dec_ctx(&x, vm)?;
            if xd.special != Special::Finite || xd.exp != 0 || xd.sign {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "logical_invert requires non-negative integer"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            if !logical_valid(vm, &ctx, &xd.coeff)? {
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            // CPython: logical_invert is logical_xor(self, 1*prec).
            let ones = BigInt::from(10).pow(ctx.prec.max(1) as u32) - BigInt::from(1);
            let coeff = logical_digit_op(&xd.coeff, &ones, ctx.prec, |a, _| a ^ 1);
            Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                sign: false, coeff, exp: 0, special: Special::Finite,
            }))
        }

        #[pymethod]
        fn next_minus(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            Ok(PyDecimal::to_py(vm, next_minus_core(vm, &ctx, &xd)?))
        }

        #[pymethod]
        fn next_plus(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            Ok(PyDecimal::to_py(vm, next_plus_core(vm, &ctx, &xd)?))
        }

        #[pymethod]
        fn next_toward(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            Ok(PyDecimal::to_py(vm, next_toward_core(vm, &ctx, &xd, &yd)?))
        }

        #[pymethod]
        fn normalize(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            // A signaling-NaN signals InvalidOperation and becomes a quiet
            // NaN, payload truncated to the context precision
            // (normalize(-sNaN010) -> -NaN10 at prec 16, ddred830).
            if xd.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "normalize with sNaN"));
                }
                let mut n = xd.clone();
                n.special = Special::NaN;
                let keep = (ctx.prec as usize).max(1);
                let digits = n.coeff_str();
                if digits.len() > keep {
                    n.coeff = digits[digits.len() - keep..]
                        .parse::<BigInt>()
                        .unwrap_or_else(|_| BigInt::from(0));
                }
                return Ok(PyDecimal::to_py(vm, n));
            }
            let mut d = xd.clone();
            if d.is_zero() {
                d.exp = 0;
            } else {
                while d.coeff_digits() > 1 && (&d.coeff % BigInt::from(10)) == BigInt::from(0) {
                    d.coeff /= BigInt::from(10);
                    d.exp += 1;
                }
            }
            let mut r = _decimal_core::apply_context(vm, &ctx, d)?;
            // Underflow to zero: CPython normalizes to plain '0' (exp 0).
            if r.is_zero() {
                r.exp = 0;
            }
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn number_class(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult<String> {
            let xd = to_dec_ctx(&x, vm)?;
            match xd.special {
                // NaN/sNaN classes carry no sign (class(+NaN12345) -> 'NaN').
                Special::NaN => return Ok("NaN".to_string()),
                Special::SignalingNaN => return Ok("sNaN".to_string()),
                Special::Infinity => return Ok(format!("{}Infinity", if xd.sign { "-" } else { "+" })),
                Special::Finite => {}
            }
            let sign = if xd.sign { "-" } else { "+" };
            if xd.is_zero() {
                return Ok(format!("{sign}Zero"));
            }
            let ctx = self.ctx();
            // Subnormal/Infinite are judged on the *adjusted* exponent
            // (exp + digits - 1), matching apply_context and clasx006.
            let adj = xd.exp + _decimal_core::coeff_digits_of(&xd.coeff) as isize - 1;
            if adj < ctx.emin {
                Ok(format!("{sign}Subnormal"))
            } else if adj > ctx.emax {
                Ok(format!("{sign}Infinite"))
            } else {
                Ok(format!("{sign}Normal"))
            }
        }

        #[pymethod]
        fn to_integral(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            self.to_integral_value(x, vm)
        }

        /// to_integral_exact: like to_integral_value, but sets the Inexact and
        /// Rounded signals when the fractional part is discarded.
        #[pymethod]
        fn to_integral_exact(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            // A signaling-NaN operand signals InvalidOperation and becomes a
            // quiet NaN (payload and sign preserved).
            if xd.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "to_integral_exact with sNaN"));
                }
                let mut n = xd.clone();
                n.special = Special::NaN;
                return Ok(PyDecimal::to_py(vm, n));
            }
            let r = if xd.exp < 0 {
                let prec = _decimal_core::coeff_digits_of(&xd.coeff) as isize + xd.exp;
                let (coeff, _exp, inexact) =
                    _decimal_core::round_to_prec(vm, &ctx, xd.sign, &xd.coeff, xd.exp, prec)?;
                // CPython signals Rounded whenever fractional digits were
                // discarded, even when the discard is exact
                // (to_integral_exact(-1.0) -> -1, Rounded; 5.0 -> 5, Rounded)
                // — but a zero operand never does (to_integral_exact(-0E-3)
                // -> -0 has no flags; only an inexact zero like -0.001 does).
                if !xd.is_zero() || inexact {
                    ctx.set_flag(vm, "Rounded")?;
                    if ctx.trap(vm, "Rounded")? {
                        return Err(new_decimal_error(vm, "Rounded", "rounded result"));
                    }
                    if inexact {
                        ctx.set_flag(vm, "Inexact")?;
                        if ctx.trap(vm, "Inexact")? {
                            return Err(new_decimal_error(vm, "Inexact", "inexact result"));
                        }
                    }
                }
                _decimal_core::Dec {
                    sign: xd.sign,
                    coeff,
                    exp: 0,
                    special: xd.special,
                }
            } else {
                _decimal_core::Dec { ..xd.clone() }
            };
            let r = _decimal_core::apply_context(vm, &ctx, r)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn to_integral_value(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            // A signaling-NaN operand signals InvalidOperation and becomes a
            // quiet NaN (payload and sign preserved).
            if xd.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "to_integral with sNaN"));
                }
                let mut n = xd.clone();
                n.special = Special::NaN;
                return Ok(PyDecimal::to_py(vm, n));
            }
            // Round the coefficient to an integral value following the
            // context's rounding mode. For a negative exponent the fractional
            // digits are discarded (round_to_prec with the integer-digit
            // precision); the result exponent is 0. For a non-negative
            // exponent the value is already integral: keep the original
            // coefficient/exponent representation (56267E+3 -> 5.6267E+7,
            // not a zero-padded plain integer).
            let r = if xd.exp < 0 {
                // Keep only the integer part: discard -exp fractional digits
                // following the context rounding. round_to_prec with
                // precision = digits + exp rounds away exactly the fractional
                // digits (prec may be 0 when the whole value is fractional,
                // e.g. 0.4 -> 0 with half_up); the result exponent is 0.
                let prec = _decimal_core::coeff_digits_of(&xd.coeff) as isize + xd.exp;
                let (coeff, _exp, _inexact) = _decimal_core::round_to_prec(
                    vm, &ctx, xd.sign, &xd.coeff, xd.exp, prec,
                )?;
                _decimal_core::Dec {
                    sign: xd.sign,
                    coeff,
                    exp: 0,
                    special: xd.special,
                }
            } else {
                // Already integral: keep the original representation.
                _decimal_core::Dec { ..xd.clone() }
            };
            let r = _decimal_core::apply_context(vm, &ctx, r)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn to_sci_string(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            // CPython: str input raises TypeError (exact object conversion).
            let d = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            // CPython preserves the value: to_sci_string does not round to the
            // context precision, so toSci(1.1123450) stays 1.1123450 even at
            // prec 6. Only the exponent presentation changes.
            let d = d;
            let mut s = d.to_string();
            // capitals=0 renders exponents with lowercase 'e'.
            if *self.capitals.lock() == 0 {
                s = s.replace('E', "e");
            }
            Ok(vm.ctx.new_str(s).into())
        }

        #[pymethod]
        fn same_quantum(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            // Same rule as the instance method (decTest samq0211/0312/0325).
            use Special::{Finite, Infinity, NaN, SignalingNaN};
            let same = match (xd.special, yd.special) {
                (NaN | SignalingNaN, NaN | SignalingNaN) => true,
                (Infinity, Infinity) => true,
                (Finite, Finite) => xd.exp == yd.exp,
                _ => false,
            };
            Ok(vm.ctx.new_bool(same).into())
        }

        #[pymethod]
        fn radix(&self, _x: OptionalArg<PyObjectRef>, vm: &VirtualMachine) -> PyResult {
            Ok(vm.ctx.new_int(BigInt::from(10)).into())
        }

        #[pymethod]
        fn canonical(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            if x.downcast_ref::<PyDecimal>().is_none() {
                return Err(vm.new_type_error("argument must be a Decimal".to_string()));
            }
            let d = to_dec(&x, vm)?;
            Ok(PyDecimal::to_py(vm, d))
        }

        #[pymethod]
        fn is_canonical(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult<bool> {
            if x.downcast_ref::<PyDecimal>().is_none() {
                return Err(vm.new_type_error("argument must be a Decimal".to_string()));
            }
            Ok(true)
        }

        #[pymethod]
        fn clear_traps(&self, vm: &VirtualMachine) {
            let keys: Vec<_> = self.traps.lock().items_vec().into_iter().map(|(k, _)| k).collect();
            for k in keys {
                let _ = self.traps.lock().set_item(k.as_object(), vm.ctx.new_bool(false).into(), vm);
            }
        }

        #[pymethod]
        fn Etiny(&self) -> isize {
            let emin = *self.emin.lock();
            let prec = *self.prec.lock();
            emin - (prec - 1)
        }

        #[pymethod]
        fn Etop(&self) -> isize {
            let emax = *self.emax.lock();
            let prec = *self.prec.lock();
            emax - (prec - 1)
        }

        #[pymethod]
        fn _apply(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            let r = _decimal_core::apply_context(vm, &ctx, xd)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        // is_* methods
        #[pymethod]
        fn is_finite(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            Ok(vm.ctx.new_bool(xd.special == Special::Finite).into())
        }
        #[pymethod]
        fn is_infinite(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            Ok(vm.ctx.new_bool(xd.special == Special::Infinity).into())
        }
        #[pymethod]
        fn is_nan(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            Ok(vm.ctx.new_bool(xd.special == Special::NaN || xd.special == Special::SignalingNaN).into())
        }
        #[pymethod]
        fn is_qnan(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            Ok(vm.ctx.new_bool(xd.special == Special::NaN).into())
        }
        #[pymethod]
        fn is_signed(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            Ok(vm.ctx.new_bool(xd.sign).into())
        }
        #[pymethod]
        fn is_snan(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            Ok(vm.ctx.new_bool(xd.special == Special::SignalingNaN).into())
        }
        #[pymethod]
        fn is_subnormal(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let emin = *self.emin.lock();
            let prec = *self.prec.lock();
            let etiny = emin - (prec - 1);
            Ok(vm.ctx.new_bool(!xd.is_zero() && xd.special == Special::Finite && xd.exp < etiny).into())
        }
        #[pymethod]
        fn is_zero(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            Ok(vm.ctx.new_bool(xd.is_zero()).into())
        }
        #[pymethod]
        fn is_normal(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let emin = *self.emin.lock();
            let prec = *self.prec.lock();
            let etiny = emin - (prec - 1);
            Ok(vm.ctx.new_bool(!xd.is_zero() && xd.special == Special::Finite && xd.exp >= etiny).into())
        }

        #[pymethod]
        fn exp(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            // Share the core with Decimal.exp; this intercepted specials with
            // its own rules (NaN signalled InvalidOperation, -Infinity came
            // back unchanged) instead of letting dec_exp decide.
            let r = _decimal_core::dec_exp(vm, &ctx, &xd)?;
            Ok(PyDecimal::to_py(vm, r))
        }
        #[pymethod]
        fn ln(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            // CPython: ln(0) = -Infinity; ln(negative) -> InvalidOperation.
            if xd.is_zero() {
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: true, coeff: BigInt::from(0), exp: 0, special: Special::Infinity,
                }));
            }
            let ctx = self.ctx();
            // A negative operand is InvalidOperation even when it is infinite:
            // ln(-Infinity) raises. Checked before is_special so -Infinity does
            // not pass through. A NaN keeps its sign instead (ln(-NaN) is -NaN).
            if xd.sign && xd.special != Special::NaN && xd.special != Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "ln of non-positive"));
                }
                // InvalidOperation yields NaN, not the operand.
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let r = _decimal_core::dec_ln(vm, &ctx, &xd)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn log10(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            // CPython: log10(0) = -Infinity; log10(negative) -> InvalidOperation.
            if xd.is_zero() {
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: true, coeff: BigInt::from(0), exp: 0, special: Special::Infinity,
                }));
            }
            let ctx = self.ctx();
            // A negative operand is InvalidOperation even when it is infinite:
            // ln(-Infinity) raises. Checked before is_special so -Infinity does
            // not pass through. A NaN keeps its sign instead (ln(-NaN) is -NaN).
            if xd.sign && xd.special != Special::NaN && xd.special != Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "log10 of non-positive"));
                }
                // InvalidOperation yields NaN, not the operand.
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let r = _decimal_core::dec_log10(vm, &ctx, &xd)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn fma(&self, x: PyObjectRef, y: PyObjectRef, z: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            // CPython: Context.fma rejects strings (TypeError).
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let zd = to_dec_ctx(&z, vm)?;
            let ctx = self.ctx();
            if let Some(r) = _decimal_core::fma_special(vm, &ctx, &xd, &yd, &zd)? {
                return Ok(PyDecimal::to_py(vm, r));
            }
            // x*y + z: single rounding of the exact product plus z
            // (see the instance fma note - the mul-first chain double-rounds).
            let prod = if xd.special == Special::Infinity || yd.special == Special::Infinity {
                let x_inf = xd.special == Special::Infinity;
                let y_inf = yd.special == Special::Infinity;
                let x_zero = !x_inf && xd.coeff == BigInt::from(0);
                let y_zero = !y_inf && yd.coeff == BigInt::from(0);
                if (x_inf && y_zero) || (y_inf && x_zero) {
                    ctx.set_flag(vm, "InvalidOperation")?;
                    if ctx.trap(vm, "InvalidOperation")? {
                        return Err(new_decimal_error(
                            vm,
                            "InvalidOperation",
                            "0 * Infinity",
                        ));
                    }
                    _decimal_core::Dec {
                        sign: false,
                        coeff: BigInt::from(0),
                        exp: 0,
                        special: Special::NaN,
                    }
                } else {
                    _decimal_core::Dec {
                        sign: xd.sign != yd.sign,
                        coeff: BigInt::from(0),
                        exp: 0,
                        special: Special::Infinity,
                    }
                }
            } else {
                _decimal_core::Dec {
                    sign: xd.sign != yd.sign,
                    coeff: &xd.coeff * &yd.coeff,
                    exp: xd.exp + yd.exp,
                    special: Special::Finite,
                }
            };
            let r = _decimal_core::dec_add(&ctx, vm, &prod, &zd)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn rotate(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            // A NaN operand propagates before the count is validated (decTest
            // rotx821-834: rotate(NaN, -Inf) and rotate(-Inf, NaN) are both
            // plain NaN with NO signal; rotx810-815 show that a non-NaN
            // operand with an infinite count is Invalid_operation).
            if xd.special == Special::SignalingNaN || yd.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "rotate with sNaN"));
                }
                let src = if xd.special == Special::SignalingNaN { &xd } else { &yd };
                return Ok(PyDecimal::to_py(vm, quiet_snan_payload(&ctx, src)));
            }
            if xd.special == Special::NaN {
                return Ok(PyDecimal::to_py(vm, xd.clone()));
            }
            if yd.special == Special::NaN {
                return Ok(PyDecimal::to_py(vm, yd.clone()));
            }
            if yd.exp != 0 || yd.special != Special::Finite {
                let ctx = self.ctx();
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "rotate requires non-negative integers"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN,
                }));
            }
            let n: i64 = yd.coeff.to_string().parse().unwrap_or(0);
            let n = if yd.sign { -n } else { n };
            let r = rotate_value(vm, &ctx, &xd, n)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn to_eng_string(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let d = to_dec_ctx(&x, vm)?;
            Ok(vm.ctx.new_str(d.to_eng_string()).into())
        }
    }

    /// Quiet a signaling NaN for the coefficient operations (rotate/shift and
    /// the remainder families): Invalid_operation has already been flagged by
    /// the caller, and the payload is truncated to the context precision
    /// keeping the LOW digits with the sign preserved - decTest rotx886
    /// rotate(11, -sNaN1234567890) at prec 5 -> -NaN67890.
    fn quiet_snan_payload(ctx: &_decimal_core::Ctx, src: &Dec) -> Dec {
        let mut n = src.clone();
        n.special = Special::NaN;
        n.exp = 0;
        let digits = n.coeff.to_string();
        let keep = (ctx.prec as usize).max(1);
        if digits.len() > keep {
            n.coeff = digits[digits.len() - keep..]
                .parse()
                .unwrap_or_else(|_| BigInt::from(0));
        }
        n
    }

    /// Shared implementation of `rotate` for both the Decimal method and the
    /// Context method. The operand is padded (or truncated) to the context
    /// precision before rotating, which is what makes rotating a single-digit
    /// value meaningful: rotate(1, 3) is 1000, not 1.
    fn rotate_value(
        vm: &VirtualMachine,
        ctx: &_decimal_core::Ctx,
        d: &Dec,
        n: i64,
    ) -> PyResult<Dec> {
        // rotate works on the coefficient and leaves the exponent alone, so a
        // negative exponent is fine (rotate(0.5, 3) is 500.0) and specials
        // pass straight through (rotate(Infinity, 3) is Infinity). Only the
        // rotation amount is validated, by the caller.
        if d.special != Special::Finite {
            return Ok(d.clone());
        }
        // CPython bounds the rotation amount to [-prec, prec].
        if !(-(ctx.prec as i64) <= n && n <= ctx.prec as i64) {
            ctx.set_flag(vm, "InvalidOperation")?;
            if ctx.trap(vm, "InvalidOperation")? {
                return Err(new_decimal_error(vm, "InvalidOperation", "rotate amount out of range"));
            }
            return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
        }

        let mut digits = d.coeff.to_string();
        let topad = ctx.prec - digits.len() as isize;
        if topad > 0 {
            digits = format!("{}{}", "0".repeat(topad as usize), digits);
        } else if topad < 0 {
            digits = digits[digits.len() - ctx.prec as usize..].to_string();
        }
        if digits.is_empty() {
            return Ok(Dec::zero());
        }
        // Positive n is a left rotation: rotdig[n:] + rotdig[:n].
        let len = digits.len() as i64;
        let cut = (((n % len) + len) % len) as usize;
        let rotated = format!("{}{}", &digits[cut..], &digits[..cut]);
        let cleaned = rotated.trim_start_matches('0');
        let coeff: BigInt = if cleaned.is_empty() {
            BigInt::from(0)
        } else {
            cleaned.parse().unwrap_or_else(|_| BigInt::from(0))
        };
        let result = Dec { sign: d.sign, coeff, exp: d.exp, special: Special::Finite };
        // decTest rotate: no case carries any signal (rotx151's result
        // 1.0000000E-1000 is subnormal-shaped but CPython flags nothing).
        // rotate is an exact coefficient operation: return as-is.
        Ok(result)
    }
    impl Representable for PyContext {
        fn repr(zelf: &Py<Self>, vm: &VirtualMachine) -> PyResult<PyStrRef> {
            Ok(vm.ctx.new_str(ctx_repr_string_impl(zelf, vm)))
        }
    }

    /// Build the CPython Context repr/str: lists only True flags/traps by
    /// diagnostic name.
    fn ctx_repr_string_impl(zelf: &PyContext, vm: &VirtualMachine) -> String {
        let repr = ctx_repr_common(
            *zelf.prec.lock(),
            *zelf.rounding.lock(),
            *zelf.emin.lock(),
            *zelf.emax.lock(),
            *zelf.capitals.lock(),
            *zelf.clamp.lock(),
            &zelf.flags.lock().clone(),
            &zelf.traps.lock().clone(),
            vm,
        );
        repr
    }

    /// Name of an exception type object used as a flags/traps dict key.
    fn exception_name(obj: &PyObject, vm: &VirtualMachine) -> String {
        if let Some(ty) = obj.downcast_ref::<PyType>() {
            ty.name().to_string()
        } else {
            obj.str(vm).map(|s| s.as_wtf8().to_string()).unwrap_or_default()
        }
    }

    fn ctx_repr_common(
        prec: isize,
        rounding: Rounding,
        emin: isize,
        emax: isize,
        capitals: isize,
        clamp: isize,
        flags: &PyDictRef,
        traps: &PyDictRef,
        vm: &VirtualMachine,
    ) -> String {
        let rounding_str = rounding.to_str();
        // CPython repr lists signals in canonical order
        // (Clamped, InvalidOperation, DivisionByZero, Inexact,
        // FloatOperation, Overflow, Rounded, Subnormal, Underflow).
        let names: Vec<(&str, &PyTypeRef)> = vec![
            ("Clamped", &exc().clamped),
            ("InvalidOperation", &exc().invalid_operation),
            ("DivisionByZero", &exc().division_by_zero),
            ("Inexact", &exc().inexact),
            ("FloatOperation", &exc().float_operation),
            ("Overflow", &exc().overflow),
            ("Rounded", &exc().rounded),
            ("Subnormal", &exc().subnormal),
            ("Underflow", &exc().underflow),
        ];
        let is_set = |d: &PyDictRef, ty: &PyTypeRef| -> bool {
            let v = d.get_item_opt(ty.as_object(), vm).ok().flatten();
            v.and_then(|x| x.try_to_value::<bool>(vm).ok()).unwrap_or(false)
        };
        let flag_names: Vec<&str> = names
            .iter()
            .filter_map(|(n, ty)| if is_set(flags, ty) { Some(*n) } else { None })
            .collect();
        let trap_names: Vec<&str> = names
            .iter()
            .filter_map(|(n, ty)| if is_set(traps, ty) { Some(*n) } else { None })
            .collect();
        format!(
            "Context(prec={prec}, rounding={rounding_str}, Emin={emin}, Emax={emax}, capitals={capitals}, clamp={clamp}, flags=[{}], traps=[{}])",
            flag_names.join(", "),
            trap_names.join(", "),
        )
    }

    impl Constructor for PyContext {
        type Args = FuncArgs;

        fn py_new(_cls: &Py<PyType>, args: FuncArgs, vm: &VirtualMachine) -> PyResult<Self> {
            let mut c = Self::new_ctx(vm, 28);
            // Context(prec, rounding, Emin, Emax, capitals, clamp) — accept
            // both positional and keyword forms (the test suite uses keyword
            // args like Context(prec=9, rounding=..., traps=...)).
            let mut pos_iter = args.args.iter();
            // Validate kwargs: only known Context params are accepted.
            const VALID_KEYS: &[&str] = &[
                "prec", "rounding", "Emin", "Emax", "capitals", "clamp",
                "flags", "traps",
            ];
            for k in args.kwargs.keys() {
                let k_str = k.to_string();
                if !VALID_KEYS.contains(&k_str.as_str()) {
                    return Err(vm.new_type_error(
                        format!("Context() got an unexpected keyword argument '{}'", k_str),
                    ));
                }
            }
            let mut get_field = |key: &str| -> Option<PyObjectRef> {
                args.get_optional_kwarg(key)
                    .or_else(|| pos_iter.next().cloned())
                    // None (explicit or from a None keyword) means "inherit
                    // the default" (CPython: Context(prec=None, ...) is the
                    // default context).
                    .filter(|v| !downcast_ref_none(v, vm))
            };
            if let Some(v) = get_field("prec") {
                let pv: isize = v.try_to_value(vm)?;
                if pv < 1 {
                    return Err(vm.new_value_error("prec must be >= 1".to_string()));
                }
                if pv > MAX_PREC {
                    return Err(vm.new_value_error("prec must be <= MAX_PREC".to_string()));
                }
                c.prec = PyMutex::new(pv);
            }
            if let Some(v) = get_field("rounding") {
                let s = v
                    .downcast_ref::<PyStr>()
                    .ok_or_else(|| vm.new_type_error("rounding must be a string".to_string()))?;
                c.rounding = PyMutex::new(
                    Rounding::from_str(&s.as_wtf8().to_string())
                        .ok_or_else(|| vm.new_type_error("invalid rounding mode".to_string()))?,
                );
            }
            if let Some(v) = get_field("Emin") {
                let ev: isize = v.try_to_value(vm)?;
                if ev > 0 {
                    return Err(vm.new_value_error("Emin must be <= 0".to_string()));
                }
                if ev < MIN_EMIN {
                    return Err(vm.new_value_error("Emin must be >= MIN_EMIN".to_string()));
                }
                c.emin = PyMutex::new(ev);
            }
            if let Some(v) = get_field("Emax") {
                let ev: isize = v.try_to_value(vm)?;
                if ev < 0 {
                    return Err(vm.new_value_error("Emax must be >= 0".to_string()));
                }
                if ev > MAX_EMAX {
                    return Err(vm.new_value_error("Emax must be <= MAX_EMAX".to_string()));
                }
                c.emax = PyMutex::new(ev);
            }
            if let Some(v) = get_field("capitals") {
                let cv: isize = v.try_to_value(vm)?;
                if cv != 0 && cv != 1 {
                    return Err(vm.new_value_error("capitals must be 0 or 1".to_string()));
                }
                c.capitals = PyMutex::new(cv);
            }
            if let Some(v) = get_field("clamp") {
                let cv: isize = v.try_to_value(vm)?;
                if cv != 0 && cv != 1 {
                    return Err(vm.new_value_error("clamp must be 0 or 1".to_string()));
                }
                c.clamp = PyMutex::new(cv);
            }
            if let Some(v) = get_field("flags") {
                let flags_guard = c.flags.lock();
                for ty in signal_types() {
                    let _ = flags_guard.set_item(ty.as_object(), vm.ctx.new_bool(false).into(), vm);
                }
                if let Some(d) = v.downcast_ref::<PyDict>() {
                    for (k, val) in d.items_vec() {
                        let on = val.try_to_value::<bool>(vm)?;
                        let _ = flags_guard.set_item(k.as_object(), vm.ctx.new_bool(on).into(), vm);
                    }
                } else if let Some(t) = v.downcast_ref::<PyTuple>() {
                    let items: Vec<PyObjectRef> = t.as_slice().to_vec();
                    validate_signal_names(&items, vm)?;
                    for item in items {
                        let _ = flags_guard.set_item(item.as_object(), vm.ctx.new_bool(true).into(), vm);
                    }
                } else if let Some(lst) = v.downcast_ref::<crate::vm::builtins::PyList>() {
                    let items: Vec<PyObjectRef> = lst.borrow_vec().to_vec();
                    validate_signal_names(&items, vm)?;
                    for item in items {
                        let _ = flags_guard.set_item(item.as_object(), vm.ctx.new_bool(true).into(), vm);
                    }
                } else {
                    return Err(vm.new_type_error("flags must be a dict, list, or tuple".to_string()));
                }
            }
            if let Some(v) = get_field("traps") {
                let traps_guard = c.traps.lock();
                // Ensure all known signals present (default False).
                for ty in signal_types() {
                    let _ = traps_guard.set_item(ty.as_object(), vm.ctx.new_bool(false).into(), vm);
                }
                if let Some(d) = v.downcast_ref::<PyDict>() {
                    for (k, val) in d.items_vec() {
                        let on = val.try_to_value::<bool>(vm)?;
                        let _ = traps_guard.set_item(k.as_object(), vm.ctx.new_bool(on).into(), vm);
                    }
                } else if let Some(t) = v.downcast_ref::<PyTuple>() {
                    let items: Vec<PyObjectRef> = t.as_slice().to_vec();
                    // __reduce__ form: tuple of exception objects to enable.
                    if !items.is_empty() && items[0].downcast_ref::<PyDecimal>().is_some() {
                        // tuple like (Decimal, ...) isn't valid here
                        return Err(vm.new_type_error("traps must be a dict, list, or tuple".to_string()));
                    }
                    validate_signal_names(&items, vm)?;
                    for item in items {
                        let _ = traps_guard.set_item(item.as_object(), vm.ctx.new_bool(true).into(), vm);
                    }
                } else if let Some(lst) = v.downcast_ref::<crate::vm::builtins::PyList>() {
                    let items: Vec<PyObjectRef> = lst.borrow_vec().to_vec();
                    validate_signal_names(&items, vm)?;
                    for item in items {
                        let _ = traps_guard.set_item(item.as_object(), vm.ctx.new_bool(true).into(), vm);
                    }
                } else {
                    return Err(vm.new_type_error("traps must be a dict, list, or tuple".to_string()));
                }
            }
            Ok(c)
        }
    }

    // The ContextManager type returned by the localcontext() function.
    // Instances can only be created from Rust (via localcontext() below);
    // direct Python-level instantiation is disallowed, mirroring CPython's
    // Py_TPFLAGS_DISALLOW_INSTANTIATION on _decimal.localcontext's result.
    #[pyclass(no_attr, name = "localcontext", module = "decimal", traverse)]
    #[derive(Debug, PyPayload)]
    struct LocalContext {
        saved: PyMutex<Option<PyObjectRef>>,
        ctx_arg: Option<PyObjectRef>,
    }

    #[pyclass(flags(DISALLOW_INSTANTIATION))]
    impl LocalContext {
        #[pymethod]
        fn __enter__(&self, vm: &VirtualMachine) -> PyResult<PyObjectRef> {
            // Save the current context for __exit__ restoration
            let current = get_current_context(vm)?;
            *self.saved.lock() = Some(current.clone());
            // Create a copy of the source context (either the provided ctx
            // or the current one), set it as current, and return it.
            // CPython's localcontext.__enter__ returns a fresh copy so
            // flag/trap modifications don't leak to the parent context.
            let source = self.ctx_arg.clone().unwrap_or(current);
            let copy = vm.call_method(&source, "copy", ())?;
            set_current_context(copy.clone());
            Ok(copy)
        }

        #[pymethod]
        fn __exit__(
            &self,
            _exc_type: PyObjectRef,
            _exc_val: PyObjectRef,
            _exc_tb: PyObjectRef,
        ) -> bool {
            if let Some(saved) = self.saved.lock().take() {
                set_current_context(saved);
            }
            false
        }
    }

    // localcontext(ctx=None, **kwargs): 'ctx' may be given as a keyword;
    // treated as the first positional argument. An explicit None is the
    // same as omitting the argument. Returns a ContextManager (LocalContext)
    // instance; the LocalContext type itself cannot be instantiated from
    // Python (DISALLOW_INSTANTIATION), matching CPython.
    #[pyfunction]
    fn localcontext(args: FuncArgs, vm: &VirtualMachine) -> PyResult<PyObjectRef> {
        let mut positional: Vec<PyObjectRef> = args.args.to_vec();
        if let Some(ctx) = args.get_optional_kwarg("ctx") {
            if positional.is_empty() {
                positional.push(ctx);
            }
        }
        let ctx_arg = if positional.is_empty()
            || positional[0].is(&vm.ctx.none())
        {
            None
        } else {
            Some(positional[0].clone())
        };
        // CPython: localcontext accepts kwargs (other than ctx) to modify
        // the context copy. If present, create a Context with them.
        let ctx_arg = if args.kwargs.is_empty() {
            ctx_arg
        } else {
            let filtered: Vec<(rustpython_common::wtf8::Wtf8Buf, PyObjectRef)> = args
                .kwargs
                .iter()
                .filter(|(k, _)| k.as_bytes() != b"ctx")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if filtered.is_empty() {
                ctx_arg
            } else {
                let ctx_cls: PyObjectRef = vm
                    .import("_decimal", 0)
                    .and_then(|m| m.get_attr("Context", vm))
                    .unwrap_or_else(|_| vm.ctx.none());
                let pos = positional.clone();
                let kw: rustpython_vm::function::KwArgs = filtered.into_iter().collect();
                Some(vm.invoke(&ctx_cls, FuncArgs::new(pos, kw))?)
            }
        };
        // The ctx argument must be a Context (or omitted). Anything else is
        // a TypeError (cpython: localcontext("xyz") -> TypeError).
        if let Some(ctx) = &ctx_arg {
            if ctx.downcast_ref::<PyContext>().is_none() {
                return Err(vm.new_type_error("context must be a Context object".to_string()));
            }
        }
        Ok(LocalContext {
            saved: PyMutex::new(None),
            ctx_arg,
        }
        .into_ref(&vm.ctx)
        .into())
    }

    #[pyfunction]
    fn getcontext(vm: &VirtualMachine) -> PyResult<PyObjectRef> {
        get_current_context(vm)
    }

    #[pyfunction]
    fn setcontext(context: PyObjectRef, vm: &VirtualMachine) -> PyResult<()> {
        if context.downcast_ref::<PyContext>().is_none() {
            return Err(vm.new_type_error("context must be a Context object".to_string()));
        }
        // CPython setcontext stores a copy; getcontext() returns a new
        // copy of the stored context each time.
        let c = vm.call_method(&context, "copy", ())?;
        set_current_context(c);
        Ok(())
    }

    #[pyfunction]
    fn IEEEContext(bits: usize, vm: &VirtualMachine) -> PyResult {
        // Return a context with precision/bounds appropriate for the given
        // IEEE 754 bit width (32, 64, 128).
        let (prec, emax) = match bits {
            32 => (7, 96),
            64 => (16, 384),
            128 => (34, 6144),
            _ => return Err(vm.new_value_error("bits must be 32, 64, or 128")),
        };
        let emin = -(emax - 1);
        let c = PyContext {
            prec: PyMutex::new(prec as isize),
            rounding: PyMutex::new(Rounding::HalfEven),
            emin: PyMutex::new(emin),
            emax: PyMutex::new(emax),
            capitals: PyMutex::new(1),
            clamp: PyMutex::new(1),
            traps: {
                let t = vm.ctx.new_dict();
                for ty in signal_types() {
                    // IEEEContext: all traps off
                    let _ = t.set_item(ty.as_object(), vm.ctx.new_bool(false).into(), vm);
                }
                PyMutex::new(t)
            },
            flags: {
                let f = vm.ctx.new_dict();
                for ty in signal_types() {
                    let _ = f.set_item(ty.as_object(), vm.ctx.new_bool(false).into(), vm);
                }
                PyMutex::new(f)
            },
        };
        Ok(c.into_ref(&vm.ctx).into())
    }

    pub(crate) fn module_exec(vm: &VirtualMachine, module: &Py<PyModule>) -> PyResult<()> {
        __module_exec(vm, module);
        init_exceptions(vm);

        let set = |name: &'static str, value: PyObjectRef| -> PyResult<()> {
            module.set_attr(name, value, vm)?;
            Ok(())
        };

        set("MAX_PREC", vm.ctx.new_int(BigInt::from(999999999999999999i64)).into())?;
        set("MAX_EMAX", vm.ctx.new_int(BigInt::from(999999999999999999i64)).into())?;
        set("MIN_EMIN", vm.ctx.new_int(BigInt::from(-999999999999999999i64)).into())?;
        set("MIN_ETINY", vm.ctx.new_int(BigInt::from(-1999999999999999997i64)).into())?;
        set("IEEE_CONTEXT_MAX_BITS", vm.ctx.new_int(BigInt::from(512i64)).into())?;
        set("HAVE_THREADS", vm.ctx.new_bool(true).into())?;
        set("HAVE_CONTEXTVAR", vm.ctx.new_bool(false).into())?;
        set("__version__", vm.ctx.new_str("1.70").into())?;
        set("__libmpdec_version__", vm.ctx.new_str("2.4.2").into())?;
        for r in [
            "ROUND_CEILING",
            "ROUND_DOWN",
            "ROUND_FLOOR",
            "ROUND_HALF_DOWN",
            "ROUND_HALF_EVEN",
            "ROUND_HALF_UP",
            "ROUND_UP",
            "ROUND_05UP",
        ] {
            // Intern the rounding string so that `C.ROUND_UP is P.ROUND_UP`
            // holds (CPython: ROUND_* are the same interned strings).
            set(r, vm.ctx.intern_str(r).to_object())?;
        }

        for (name, ty) in [
            ("DecimalException", exc().decimal_exception.clone()),
            ("Clamped", exc().clamped.clone()),
            ("InvalidOperation", exc().invalid_operation.clone()),
            ("ConversionSyntax", exc().conversion_syntax.clone()),
            ("DivisionImpossible", exc().division_impossible.clone()),
            ("DivisionUndefined", exc().division_undefined.clone()),
            ("InvalidContext", exc().invalid_context.clone()),
            ("DivisionByZero", exc().division_by_zero.clone()),
            ("Inexact", exc().inexact.clone()),
            ("Overflow", exc().overflow.clone()),
            ("Underflow", exc().underflow.clone()),
            ("Rounded", exc().rounded.clone()),
            ("Subnormal", exc().subnormal.clone()),
            ("FloatOperation", exc().float_operation.clone()),
        ] {
            set(name, ty.into())?;
        }

        let default: PyObjectRef = PyContext::new_ctx(vm, 28).into_ref(&vm.ctx).into();
        *DEFAULT_CTX.lock() = Some(default.clone());
        set("DefaultContext", default)?;

        let basic: PyObjectRef = PyContext::new_ctx(vm, 9).into_ref(&vm.ctx).into();
        // CPython BasicContext traps: InvalidOperation, DivisionByZero, Overflow,
        // Underflow, Clamped (new_ctx enables the first three).
        if let Some(pc) = basic.downcast_ref::<PyContext>() {
            let _ = pc.traps.lock().set_item(exc().underflow.as_object(), vm.ctx.new_bool(true).into(), vm);
            let _ = pc.traps.lock().set_item(exc().clamped.as_object(), vm.ctx.new_bool(true).into(), vm);
        }
        set("BasicContext", basic)?;

        let extended: PyObjectRef = PyContext::new_ctx(vm, 9).into_ref(&vm.ctx).into();
        if let Some(pc) = extended.downcast_ref::<PyContext>() {
            let keys: Vec<_> = pc.traps.lock().items_vec().into_iter().map(|(k, _)| k).collect();
            for k in keys {
                let _ = pc.traps.lock().set_item(k.as_object(), vm.ctx.new_bool(false).into(), vm);
            }
        }
        set("ExtendedContext", extended)?;

        // Enable subclassing: set BASETYPE flag on Decimal and Context types.
        for name in ["Decimal", "Context"] {
            if let Ok(cls) = module.get_attr(name, vm) {
                if let Some(ty) = cls.downcast_ref::<PyType>() {
                    // Safety: this runs during module initialization before
                    // any user code can observe the type, and BASETYPE is a
                    // one-way flag that only needs to be set once.
                    let slots_ptr = &ty.slots as *const PyTypeSlots as *mut PyTypeSlots;
                    unsafe { (*slots_ptr).flags |= PyTypeFlags::BASETYPE; }
                }
            }
        }

        // Register Decimal as a virtual subclass of numbers.Number
        // (CPython: Decimal.register(Number)). The actual registration
        // goes through the abc machinery.
        if let (Ok(number_mod), Ok(decimal_cls)) = (
            vm.import("numbers", 0),
            module.get_attr("Decimal", vm),
        ) {
            if let Ok(number_cls) = number_mod.get_attr("Number", vm) {
                let _ = vm.call_method(&number_cls, "register", (decimal_cls,));
            }
        }

        // CPython exposes these types with __module__ == 'decimal' (not
        // '_decimal'); the pyclass declarations use module="decimal" so
        // pickle round-trips resolve through the swapped sys.modules['decimal']
        // (C <-> P interchangeability in the suite).

        // Define DecimalTuple as a namedtuple: (sign, digits, exponent).
        // as_tuple() returns one, matching CPython (_decimal.DecimalTuple).
        // Created once per process and shared across module instances.
        {
            let mut guard = DECIMAL_TUPLE.lock();
            if guard.is_none() {
                let collections_mod = vm.import("collections", 0)?;
                let nt = collections_mod.get_attr("namedtuple", vm)?;
                let dt_name: PyObjectRef = vm.ctx.new_str("DecimalTuple").into();
                let dt_fields: PyObjectRef = vm.ctx.new_str("sign digits exponent").into();
                let dt = vm.invoke(&nt, (dt_name, dt_fields))?;
                // CPython's _decimal.DecimalTuple.__module__ is 'decimal'; set it
                // so pickle resolves through sys.modules['decimal'].
                let dec_mod: PyObjectRef = vm.ctx.new_str("decimal").into();
                let _ = dt.set_attr("__module__", dec_mod, vm);
                *guard = Some(dt.clone());
                set("DecimalTuple", dt)?;
            } else {
                let dt = guard.clone().unwrap();
                set("DecimalTuple", dt)?;
            }
        }

        Ok(())
    }
}





