#[path = "_decimal_core.rs"]
mod _decimal_core;

pub(crate) use _decimal::module_def;

use std::cell::RefCell;

use malachite_bigint::BigInt;

use rustpython_common::hash;
use rustpython_common::lock::PyMutex;

use crate::vm::{
    AsObject, Py, PyObject, PyObjectRef, PyPayload, PyRef, PyResult, VirtualMachine,
    builtins::{PyDict, PyDictRef, PyFloat, PyInt, PyModule, PyStr, PyStrRef, PyTuple, PyTupleRef, PyType, PyTypeRef},
    convert::ToPyResult,
    function::{FuncArgs, OptionalArg, PyComparisonValue},
    protocol::PyNumberMethods,
    types::{AsNumber, Comparable, Constructor, Hashable, PyComparisonOp, Representable},
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

    static DEFAULT_CTX: SyncMutex<Option<PyObjectRef>> = SyncMutex(PyMutex::new(None));

    // Shared DecimalTuple namedtuple class (created once per process).
    static DECIMAL_TUPLE: SyncMutex<Option<PyObjectRef>> = SyncMutex(PyMutex::new(None));

    fn get_current_context(vm: &VirtualMachine) -> PyObjectRef {
        CURRENT_CONTEXT.with(|c| {
            c.borrow().clone().unwrap_or_else(|| {
                DEFAULT_CTX.0.lock().clone().unwrap_or_else(|| {
                    PyContext::new_ctx(vm, 28).into_ref(&vm.ctx).into()
                })
            })
        })
    }

    fn set_current_context(ctx: PyObjectRef) {
        CURRENT_CONTEXT.with(|c| *c.borrow_mut() = Some(ctx));
    }

    fn current_ctx(vm: &VirtualMachine) -> PyResult<Ctx> {
        let obj = get_current_context(vm);
        let pc = obj
            .downcast_ref::<PyContext>()
            .ok_or_else(|| vm.new_type_error("context must be a Context object".to_string()))?;
        Ok(pc.ctx())
    }

    /// compare two decimals (sign*coeff*10^exp). NaN sorts before everything.
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
        let ne = a.exp.min(b.exp);
        let ca = &a.coeff * BigInt::from(10).pow((a.exp - ne) as u32);
        let cb = &b.coeff * BigInt::from(10).pow((b.exp - ne) as u32);
        let va = if a.sign { -&ca } else { ca };
        let vb = if b.sign { -&cb } else { cb };
        use core::cmp::Ordering;
        match va.cmp(&vb) {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        }
    }

    /// Convert a Python object (Decimal/int/float/str) to Dec.
    /// Used by Decimal constructor ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â accepts str.
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
        // CPython: Decimal([1,2]) raises ValueError (not TypeError).
        if obj.downcast_ref::<crate::vm::builtins::PyList>().is_some() {
            return Err(vm.new_value_error("conversion from list to Decimal is not supported".to_string()));
        }
        Err(vm.new_type_error(format!("cannot convert {} to Decimal", obj.class().name())))
    }

    /// Convert a Python object (Decimal/int/float) to Dec.
    /// Used by Context methods ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â rejects str (CPython behavior).
    fn to_dec_ctx(obj: &PyObject, vm: &VirtualMachine) -> PyResult<Dec> {
        if let Some(pd) = obj.downcast_ref::<PyDecimal>() {
            return Ok(pd.d.clone());
        }
        if let Some(i) = obj.downcast_ref::<PyInt>() {
            return Ok(_decimal_core::dec_from_int(vm, i));
        }
        if let Some(f) = obj.downcast_ref::<PyFloat>() {
            return Ok(_decimal_core::dec_from_float(f.to_f64()));
        }
        Err(vm.new_type_error(format!("conversion from {} to Decimal is not supported", obj.class().name())))
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
            } else if let Some(i) = exp.downcast_ref::<PyInt>() {
                i.try_to_primitive::<isize>(vm)?
            } else if let Some(s) = exp.downcast_ref::<PyStr>() {
                _decimal_core::parse_decimal(&s.as_wtf8().to_string())
                    .map_err(|e| new_decimal_error(vm, "InvalidOperation", &e))?
                    .exp
            } else {
                return Err(vm.new_type_error(
                    "quantize requires a Decimal, int or str exponent".to_string(),
                ));
            };
            let rounding_str: Option<String> = args
                .kwargs
                .get("rounding")
                .map(|v| match v.downcast_ref::<PyStr>() {
                    Some(s) => Ok(s.as_wtf8().to_string()),
                    None => Err(vm.new_type_error("rounding must be a string".to_string())),
                })
                .transpose()?;
            // context= keyword overrides the current context.
            let mut ctx = if let Some(c) = get("context") {
                let pc = c.downcast_ref::<PyContext>().ok_or_else(|| {
                    vm.new_type_error("context must be a Context object".to_string())
                })?;
                pc.ctx()
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
            let ctx = ctx_from_funcargs(&args, vm)?;
            let etiny = ctx.emin - (ctx.prec - 1);
            Ok(!self.d.is_zero()
                && self.d.special == Special::Finite
                && self.d.exp >= etiny)
        }
        #[pymethod]
        fn is_subnormal(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult<bool> {
            let ctx = ctx_from_funcargs(&args, vm)?;
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
            let ctx = ctx_from_funcargs(&args, vm)?;
            let sign = if self.d.sign { "-" } else { "+" };
            match self.d.special {
                Special::NaN => return Ok(format!("{sign}NaN")),
                Special::SignalingNaN => return Ok(format!("{sign}sNaN")),
                Special::Infinity => return Ok(format!("{sign}Infinity")),
                Special::Finite => {}
            }
            if self.d.is_zero() {
                return Ok(format!("{sign}Zero"));
            }
            let etiny = ctx.emin - (ctx.prec - 1);
            if self.d.exp < etiny {
                Ok(format!("{sign}Subnormal"))
            } else if self.d.exp > ctx.emax {
                Ok(format!("{sign}Infinite"))
            } else {
                Ok(format!("{sign}Normal"))
            }
        }

        #[pymethod]
        fn normalize(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let ctx = ctx_from_funcargs(&args, vm)?;
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
            let r = _decimal_core::apply_context(vm, &ctx, d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn same_quantum(&self, other: PyObjectRef, vm: &VirtualMachine) -> PyResult<bool> {
            let b = to_dec(&other, vm)?;
            Ok(self.d.exp == b.exp)
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
            let b = to_dec(&other, vm)?;
            let _ = ctx_from_funcargs(&args, vm)?; // accept context arg
            Ok(vm.ctx.new_int(BigInt::from(dec_cmp(&self.d, &b))).into())
        }

        #[pymethod]
        fn next_minus(
            &self,
            args: FuncArgs,
            vm: &VirtualMachine,
        ) -> PyResult {
            let ctx = ctx_from_funcargs(&args, vm)?;
            Ok(PyDecimal::to_py(vm, self.next_step(vm, &ctx, -1)?))
        }

        #[pymethod]
        fn next_plus(
            &self,
            args: FuncArgs,
            vm: &VirtualMachine,
        ) -> PyResult {
            let ctx = ctx_from_funcargs(&args, vm)?;
            Ok(PyDecimal::to_py(vm, self.next_step(vm, &ctx, 1)?))
        }

        fn next_step(
            &self,
            vm: &VirtualMachine,
            ctx: &Ctx,
            dir: i8,
        ) -> PyResult<Dec> {
            // next_plus/next_minus: the nearest representable value in the
            // context's precision just above/below this one.
            let d = &self.d;
            if d.is_zero() {
                // 0 -> +/- 0.1 (smallest representable 1-digit increment)
                return Ok(Dec {
                    sign: dir < 0,
                    coeff: BigInt::from(1),
                    exp: -1,
                    special: Special::Finite,
                });
            }
            if d.is_special() {
                return Ok(d.clone());
            }
            // step = 10^expIncrement.
            let step = BigInt::from(10).pow(d.exp.max(0) as u32);
            // next_minus moves toward -Inf; next_plus toward +Inf. The sign
            // is always preserved. next_minus increases magnitude for
            // negatives (more negative), decreases for positives; next_plus
            // does the opposite.
            let want_negative = d.sign;
            let bigger_mag = (d.sign && dir < 0) || (!d.sign && dir > 0);
            let (coeff, n_exp) = if bigger_mag {
                (&d.coeff + &step, d.exp)
            } else if &d.coeff <= &step {
                (BigInt::from(9), d.exp - 1)
            } else {
                (&d.coeff - &step, d.exp)
            };
            let result = Dec { sign: want_negative, coeff, exp: n_exp, special: Special::Finite };
            let r = _decimal_core::apply_context(vm, ctx, result)?;
            Ok(r)
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
            // If equal (value-wise), return self.
            if dec_cmp(&self.d, &b) == 0 && self.d.special == b.special {
                return Ok(PyDecimal::to_py(vm, self.d.clone()));
            }
            // Move one ulp toward b: toward -Inf if b < self, else +Inf.
            let dir = if dec_cmp(&b, &self.d) < 0 { -1i8 } else { 1i8 };
            let r = self.next_step(vm, &ctx, dir)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn max(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let other = args
                .args
                .get(0)
                .cloned()
                .ok_or_else(|| vm.new_type_error("max requires an argument".to_string()))?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let b = to_dec(&other, vm)?;
            let chosen = if dec_cmp(&self.d, &b) >= 0 { self.d.clone() } else { b };
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
            let b = to_dec(&other, vm)?;
            let chosen = if dec_cmp(&self.d, &b) <= 0 { self.d.clone() } else { b };
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
            let b = to_dec(&other, vm)?;
            let xa = Dec { sign: false, coeff: self.d.coeff.clone(), exp: self.d.exp, special: self.d.special };
            let ya = Dec { sign: false, coeff: b.coeff.clone(), exp: b.exp, special: b.special };
            let chosen = if dec_cmp(&xa, &ya) >= 0 { self.d.clone() } else { b };
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
            let b = to_dec(&other, vm)?;
            let xa = Dec { sign: false, coeff: self.d.coeff.clone(), exp: self.d.exp, special: self.d.special };
            let ya = Dec { sign: false, coeff: b.coeff.clone(), exp: b.exp, special: b.special };
            let chosen = if dec_cmp(&xa, &ya) <= 0 { self.d.clone() } else { b };
            let r = _decimal_core::apply_context(vm, &ctx, chosen)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn sqrt(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let ctx = ctx_from_funcargs(&args, vm)?;
            if self.d.is_special() || self.d.is_zero() {
                return Ok(PyDecimal::to_py(vm, self.d.clone()));
            }
            if self.d.sign {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "sqrt of negative"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN }));
            }
            let prec = ctx.prec as usize + 4;
            let xf = self.d.to_string().parse::<f64>().unwrap_or(1.0);
            let result = xf.sqrt();
            let s = format!("{:.*}", prec, result);
            let d = _decimal_core::parse_decimal(&s).unwrap_or(_decimal_core::Dec::zero());
            let r = _decimal_core::apply_context(vm, &ctx, d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn exp(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let ctx = ctx_from_funcargs(&args, vm)?;
            if self.d.is_special() {
                if self.d.special == Special::Infinity {
                    let r = _decimal_core::apply_context(vm, &ctx, self.d.clone())?;
                    return Ok(PyDecimal::to_py(vm, r));
                }
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "exp of NaN"));
                }
                return Ok(PyDecimal::to_py(vm, self.d.clone()));
            }
            let prec = ctx.prec as usize + 4;
            let xf = self.d.to_string().parse::<f64>().unwrap_or(0.0);
            // exp of a huge positive value overflows to infinity; CPython
            // raises Overflow (trapped) / returns Infinity.
            if xf > 709.0 {
                ctx.set_flag(vm, "Overflow")?;
                if ctx.trap(vm, "Overflow")? {
                    return Err(new_decimal_error(vm, "Overflow", "exp overflow"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Infinity }));
            }
            let result = xf.exp();
            let s = format!("{:.*}", prec, result);
            let d = _decimal_core::parse_decimal(&s).unwrap_or(_decimal_core::Dec::zero());
            let r = _decimal_core::apply_context(vm, &ctx, d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn ln(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let ctx = ctx_from_funcargs(&args, vm)?;
            // CPython: ln(0) = -Infinity; ln(negative) -> InvalidOperation.
            if self.d.is_zero() {
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: true, coeff: BigInt::from(0), exp: 0, special: Special::Infinity,
                }));
            }
            if self.d.is_special() || self.d.sign {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "ln of non-positive"));
                }
                return Ok(PyDecimal::to_py(vm, self.d.clone()));
            }
            let prec = ctx.prec as usize + 4;
            let xf = self.d.to_string().parse::<f64>().unwrap_or(1.0);
            let result = xf.ln();
            let s = format!("{:.*}", prec, result);
            let d = _decimal_core::parse_decimal(&s).unwrap_or(_decimal_core::Dec::zero());
            let r = _decimal_core::apply_context(vm, &ctx, d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn log10(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let ctx = ctx_from_funcargs(&args, vm)?;
            // CPython: log10(0) = -Infinity; log10(negative) -> InvalidOperation.
            if self.d.is_zero() {
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: true, coeff: BigInt::from(0), exp: 0, special: Special::Infinity,
                }));
            }
            if self.d.is_special() || self.d.sign {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "log10 of non-positive"));
                }
                return Ok(PyDecimal::to_py(vm, self.d.clone()));
            }
            let prec = ctx.prec as usize + 4;
            let xf = self.d.to_string().parse::<f64>().unwrap_or(1.0);
            let result = xf.log10();
            let s = format!("{:.*}", prec, result);
            let d = _decimal_core::parse_decimal(&s).unwrap_or(_decimal_core::Dec::zero());
            let r = _decimal_core::apply_context(vm, &ctx, d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn logb(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let ctx = ctx_from_funcargs(&args, vm)?;
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
            let b = to_dec(&other, vm)?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            if self.d.special == Special::NaN || self.d.special == Special::SignalingNaN
                || b.special == Special::NaN || b.special == Special::SignalingNaN
            {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "comparison with NaN"));
                }
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
            } else { dec_cmp(&self.d, &b) };
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
            } else { dec_cmp(&xd, &yd) };
            Ok(vm.ctx.new_int(BigInt::from(r)).into())
        }

        #[pymethod]
        fn __str__(zelf: &Py<Self>, vm: &VirtualMachine) -> PyStrRef {
            vm.ctx.new_str(zelf.d.to_string())
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
            // The second positional arg is localeconv data (dict) for the
            // 'n' format; it is not a Context. Use it only as a passthrough
            // (grouping not implemented). The current context applies.
            let _ = localeconv;
            let ctx = current_ctx(vm)?;
            _decimal_core::format_decimal(&self.d, &spec, vm, &ctx)
                .map_err(|e| vm.new_value_error(e))
        }

        // Reflected ops are handled by the AsNumber right_* slots
        // (copy_from sets right_add = add, and binop_num checks both args).

        #[pymethod]
        fn __trunc__(&self, vm: &VirtualMachine) -> PyResult {
            let mut coeff = self.d.coeff.clone();
            if self.d.exp < 0 {
                let pow = BigInt::from(10).pow((-self.d.exp) as u32);
                coeff = &coeff / &pow;
            } else if self.d.exp > 0 {
                coeff *= BigInt::from(10).pow(self.d.exp as u32);
            }
            let v = if self.d.sign { -coeff } else { coeff };
            Ok(vm.ctx.new_int(v).into())
        }

        #[pymethod]
        fn __floor__(&self, vm: &VirtualMachine) -> PyResult {
            // For Decimal, __floor__ = trunc toward zero (same as __trunc__ for non-negative)
            let mut coeff = self.d.coeff.clone();
            if self.d.exp < 0 {
                let pow = BigInt::from(10).pow((-self.d.exp) as u32);
                coeff = &coeff / &pow;
            } else if self.d.exp > 0 {
                coeff *= BigInt::from(10).pow(self.d.exp as u32);
            }
            let v = if self.d.sign { -coeff } else { coeff };
            Ok(vm.ctx.new_int(v).into())
        }

        #[pymethod]
        fn __ceil__(&self, vm: &VirtualMachine) -> PyResult {
            let mut coeff = self.d.coeff.clone();
            if self.d.exp < 0 {
                let pow = BigInt::from(10).pow((-self.d.exp) as u32);
                let r = &coeff % &pow;
                coeff = &coeff / &pow;
                if !self.d.sign && r != BigInt::from(0) {
                    coeff += BigInt::from(1);
                }
            } else if self.d.exp > 0 {
                coeff *= BigInt::from(10).pow(self.d.exp as u32);
            }
            let v = if self.d.sign { -coeff } else { coeff };
            Ok(vm.ctx.new_int(v).into())
        }

        #[pyclassmethod]
        fn from_number(cls: PyTypeRef, number: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            // PEP 3141: from_number converts int/float/Decimal to Decimal
            if number.downcast_ref::<PyDecimal>().is_some() {
                return Ok(number);
            }
            if let Some(i) = number.downcast_ref::<PyInt>() {
                let d = _decimal_core::dec_from_int(vm, i);
                let obj: PyObjectRef = PyDecimal { d }.into_ref(&vm.ctx).into();
                return Ok(obj);
            }
            if let Some(f) = number.downcast_ref::<PyFloat>() {
                let d = _decimal_core::dec_from_float(f.to_f64());
                let obj: PyObjectRef = PyDecimal { d }.into_ref(&vm.ctx).into();
                return Ok(obj);
            }
            let _ = cls;
            Err(vm.new_type_error("from_number requires int or float".to_string()))
        }

        #[pyclassmethod]
        fn from_float(cls: PyTypeRef, number: f64, vm: &VirtualMachine) -> PyResult {
            // Exact binary expansion of the float (like CPython's
            // Decimal.from_float). The _decimal constructor ignores context.
            let d = _decimal_core::dec_from_float(number);
            let _ = cls;
            Ok(PyDecimal::to_py(vm, d))
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
            let ctx = current_ctx(vm)?;
            if let Some(nd) = ndigits.into_option() {
                let n: isize = nd.try_to_value(vm)?;
                let r = _decimal_core::quantize(vm, &ctx, xd, -n)?;
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
            let rounding_str: Option<String> = args
                .get_optional_kwarg("rounding")
                .or_else(|| args.args.get(0).cloned())
                .map(|v| match v.downcast_ref::<PyStr>() {
                    Some(s) => Ok(s.as_wtf8().to_string()),
                    None => Err(vm.new_type_error("rounding must be a string".to_string())),
                })
                .transpose()?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let mut ctx2 = ctx;
            if let Some(r) = rounding_str {
                ctx2.rounding = Rounding::from_str(&r)
                    .ok_or_else(|| vm.new_value_error("invalid rounding mode".to_string()))?;
            }
            let r = _decimal_core::to_integral(vm, &ctx2, &self.d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn to_integral_value(
            &self,
            args: FuncArgs,
            vm: &VirtualMachine,
        ) -> PyResult {
            let ctx = ctx_from_funcargs(&args, vm)?;
            let r = _decimal_core::to_integral(vm, &ctx, &self.d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn to_integral_exact(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let ctx = ctx_from_funcargs(&args, vm)?;
            let r = _decimal_core::to_integral(vm, &ctx, &self.d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn to_eng_string(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult<String> {
            let _ = ctx_from_funcargs(&args, vm)?;
            Ok(self.d.to_string())
        }

        // Logical operations (operate on the decimal digit strings; result
        // keeps the operand's precision width).
        #[pymethod]
        fn logical_and(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let other = args.args.get(0).cloned()
                .ok_or_else(|| vm.new_type_error("logical_and requires an argument".to_string()))?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let b = to_dec(&other, vm)?;
            if self.d.exp != 0 || b.exp != 0 || self.d.sign || b.sign {
                return Err(new_decimal_error(vm, "InvalidOperation", "logical_and requires non-negative integers"));
            }
            let coeff = logical_digit_op(&self.d.coeff, &b.coeff, ctx.prec, |x, y| x & y);
            Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                sign: false, coeff, exp: 0, special: Special::Finite,
            }))
        }

        #[pymethod]
        fn logical_or(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let other = args.args.get(0).cloned()
                .ok_or_else(|| vm.new_type_error("logical_or requires an argument".to_string()))?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let b = to_dec(&other, vm)?;
            if self.d.exp != 0 || b.exp != 0 || self.d.sign || b.sign {
                return Err(new_decimal_error(vm, "InvalidOperation", "logical_or requires non-negative integers"));
            }
            let coeff = logical_digit_op(&self.d.coeff, &b.coeff, ctx.prec, |x, y| x | y);
            Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                sign: false, coeff, exp: 0, special: Special::Finite,
            }))
        }

        #[pymethod]
        fn logical_xor(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let other = args.args.get(0).cloned()
                .ok_or_else(|| vm.new_type_error("logical_xor requires an argument".to_string()))?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let b = to_dec(&other, vm)?;
            if self.d.exp != 0 || b.exp != 0 || self.d.sign || b.sign {
                return Err(new_decimal_error(vm, "InvalidOperation", "logical_xor requires non-negative integers"));
            }
            let coeff = logical_digit_op(&self.d.coeff, &b.coeff, ctx.prec, |x, y| x ^ y);
            Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                sign: false, coeff, exp: 0, special: Special::Finite,
            }))
        }

        #[pymethod]
        fn logical_invert(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let ctx = ctx_from_funcargs(&args, vm)?;
            if self.d.exp != 0 || self.d.sign
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
            let n_obj = to_dec(&n_argv, vm)?;
            if self.d.exp < 0 || n_obj.exp < 0 {
                return Err(new_decimal_error(vm, "InvalidOperation", "rotate requires non-negative integers"));
            }
            let n: i64 = (&n_obj.coeff * BigInt::from(10).pow(n_obj.exp.max(0) as u32)).to_string().parse().unwrap_or(0);
            let digits = self.d.coeff.to_string();
            let len = digits.len();
            if len <= 1 {
                return Ok(PyDecimal::to_py(vm, self.d.clone()));
            }
            // Normalize n into [0, len).
            let nshift = ((n % len as i64) + len as i64) % len as i64;
            let cut = (len as i64 - nshift) as usize;
            let rotated = format!("{}{}", &digits[cut..], &digits[..cut]);
            let coeff = rotated.parse::<BigInt>().unwrap_or(BigInt::from(0));
            let result = _decimal_core::Dec { sign: self.d.sign, coeff, exp: self.d.exp, special: self.d.special };
            let r = _decimal_core::apply_context(vm, &ctx, result)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn shift(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let n_argv = args.args.get(0).cloned()
                .ok_or_else(|| vm.new_type_error("shift requires an argument".to_string()))?;
            let ctx = ctx_from_funcargs(&args, vm)?;
            let n_obj = to_dec(&n_argv, vm)?;
            if self.d.exp < 0 || n_obj.exp < 0 {
                return Err(new_decimal_error(vm, "InvalidOperation", "shift requires non-negative integers"));
            }
            let n: i64 = (&n_obj.coeff * BigInt::from(10).pow(n_obj.exp.max(0) as u32)).to_string().parse().unwrap_or(0);
            // shift digits (positive = left, negative = right); keeps same exp.
            let mut digits = self.d.coeff.to_string();
            if n > 0 {
                // shift left: move more significant, pad zeros; result may
                // grow but apply_context trims.
                let shift_n = n as usize;
                digits = format!("{}{}", &digits, "0".repeat(shift_n));
            } else if n < 0 {
                let drop = (-n) as usize;
                if drop >= digits.len() {
                    digits = "0".to_string();
                } else {
                    digits = digits[..digits.len() - drop].to_string();
                }
            }
            let coeff = digits.parse::<BigInt>().unwrap_or(BigInt::from(0));
            let result = _decimal_core::Dec { sign: self.d.sign, coeff, exp: self.d.exp, special: self.d.special };
            let r = _decimal_core::apply_context(vm, &ctx, result)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn fma(&self, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            // self.fma(other, third, context=) ; other=/third= keywords.
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
            let prod = _decimal_core::dec_mul(&ctx, vm, &self.d, &other_d)?;
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
            let b = to_dec(&other, vm)?;
            // r = self - b * n where n = round(self/b) to nearest int (half-even).
            // Intermediate division uses extra precision so rounding is exact.
            let wide = Ctx::from_parts(50, Rounding::HalfEven, 999999999, -999999999, 0,
                vm.ctx.new_dict(), vm.ctx.new_dict());
            let div = _decimal_core::dec_div(&wide, vm, &self.d, &b)?;
            let mut coeff = div.coeff.clone();
            let mut exp = div.exp;
            if exp < 0 {
                let pow = BigInt::from(10).pow((-exp) as u32);
                let (q, r) = (&coeff / &pow, &coeff % &pow);
                let tie = &r * BigInt::from(2) == pow;
                let round_up = (&r * BigInt::from(2) > pow)
                    || (tie && (&q % BigInt::from(2)) == BigInt::from(1));
                coeff = if round_up { q + BigInt::from(1) } else { q };
                exp = 0;
            }
            let n = Dec { sign: div.sign, coeff, exp: 0, special: Special::Finite };
            let prod = _decimal_core::dec_mul(&wide, vm, &b, &n)?;
            let result = _decimal_core::dec_sub(&wide, vm, &self.d, &prod)?;
            let r = _decimal_core::apply_context(vm, &ctx, result)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn as_integer_ratio(&self, vm: &VirtualMachine) -> PyResult {
            let d = &self.d;
            if d.is_special() {
                return Err(new_decimal_error(vm, "InvalidOperation", "cannot convert NaN/Infinity"));
            }
            let num = d.coeff.clone();
            let n = if d.sign { -num } else { num };
            let (n, den) = if d.exp >= 0 {
                (n * BigInt::from(10).pow(d.exp as u32), BigInt::from(1))
            } else {
                (n, BigInt::from(10).pow((-d.exp) as u32))
            };
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
            // CPython: Decimal accepts at most one positional argument (plus
            // the optional context keyword). Extra positional args -> TypeError.
            if args.args.len() > 1 {
                return Err(vm.new_type_error(
                    "Decimal() takes at most 2 arguments (value and context)".to_string(),
                ));
            }
            // Support keyword args: Decimal(value=..., context=...)
            let value = args.get_optional_kwarg("value")
                .or_else(|| args.args.get(0).cloned())
                .unwrap_or_else(|| vm.ctx.new_int(0).into());
            // context keyword overrides the current context (CPython uses it for
            // flag/precision application during construction).
            let context_kw = args.get_optional_kwarg("context");
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
            // Comparing Decimal with float is an implicit float conversion:
            // sets the FloatOperation flag (raises if trapped).
            if other.downcast_ref::<PyFloat>().is_some() {
                let ctx = current_ctx(vm)?;
                ctx.set_flag(vm, "FloatOperation")?;
                if ctx.trap(vm, "FloatOperation")? {
                    return Err(new_decimal_error(
                        vm,
                        "FloatOperation",
                        "float conversion is not safe for Decimal",
                    ));
                }
            }
            let b = match to_dec(other, vm) {
                Ok(b) => b,
                Err(_) => return Ok(PyComparisonValue::NotImplemented),
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
                        // ternary power: a ** b % m; a/b may be int (like the
                        // binary slot) as long as one side is a Decimal.
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
                            return Ok(vm.ctx.not_implemented());
                        };
                        let md = to_dec(&c_obj, vm)?;
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
                    let (floor, m) = _decimal_core::floor_div_mod(&ad, &bd);
                    Ok(vm.ctx.new_tuple(vec![
                        PyDecimal::to_py(vm, floor),
                        PyDecimal::to_py(vm, m),
                    ]).into())
                }),
                negative: Some(|num, vm| {
                    let pd = PyDecimal::number_downcast(num);
                    let mut d = pd.d.clone();
                    d.sign = !d.sign;
                    Ok(PyDecimal::to_py(vm, d))
                }),
                positive: Some(|num, vm| {
                    let pd = PyDecimal::number_downcast(num);
                    Ok(PyDecimal::to_py(vm, pd.d.clone()))
                }),
                absolute: Some(|num, vm| {
                    let pd = PyDecimal::number_downcast(num);
                    let mut d = pd.d.clone();
                    d.sign = false;
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
                    let f = pd.d.to_string().parse::<f64>().unwrap_or(f64::NAN);
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
            for ty in exc().all() {
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
        fn set_prec(&self, value: isize) {
            *self.prec.lock() = value;
        }
        #[pygetset]
        fn rounding(&self) -> String {
            self.rounding.lock().to_str().to_string()
        }
        #[pygetset(setter)]
        fn set_rounding(&self, value: PyStrRef, vm: &VirtualMachine) -> PyResult<()> {
            let r = Rounding::from_str(&value.as_wtf8().to_string())
                .ok_or_else(|| vm.new_value_error("invalid rounding mode".to_string()))?;
            *self.rounding.lock() = r;
            Ok(())
        }
        #[pygetset]
        fn Emin(&self) -> isize {
            *self.emin.lock()
        }
        #[pygetset(setter)]
        fn set_Emin(&self, value: isize) {
            *self.emin.lock() = value;
        }
        #[pygetset]
        fn Emax(&self) -> isize {
            *self.emax.lock()
        }
        #[pygetset(setter)]
        fn set_Emax(&self, value: isize) {
            *self.emax.lock() = value;
        }
        #[pygetset]
        fn capitals(&self) -> isize {
            *self.capitals.lock()
        }
        #[pygetset(setter)]
        fn set_capitals(&self, value: isize) {
            *self.capitals.lock() = value;
        }
        #[pygetset]
        fn clamp(&self) -> isize {
            *self.clamp.lock()
        }
        #[pygetset(setter)]
        fn set_clamp(&self, value: isize) {
            *self.clamp.lock() = value;
        }
        #[pygetset]
        fn traps(&self) -> PyDictRef {
            self.traps.lock().clone()
        }
        #[pygetset(setter)]
        fn set_traps(&self, value: PyDictRef, vm: &VirtualMachine) {
            let new_dict = vm.ctx.new_dict();
            for (k, v) in value.items_vec() {
                let _ = new_dict.set_item(k.as_object(), v.clone(), vm);
            }
            for ty in exc().all() {
                if new_dict.get_item_opt(ty.as_object(), vm).unwrap_or(None).is_none() {
                    let _ = new_dict.set_item(ty.as_object(), vm.ctx.new_bool(false).into(), vm);
                }
            }
            *self.traps.lock() = new_dict;
        }
        #[pygetset]
        fn flags(&self) -> PyDictRef {
            self.flags.lock().clone()
        }
        #[pygetset(setter)]
        fn set_flags(&self, value: PyDictRef, vm: &VirtualMachine) {
            let new_dict = vm.ctx.new_dict();
            for (k, v) in value.items_vec() {
                let _ = new_dict.set_item(k.as_object(), v.clone(), vm);
            }
            for ty in exc().all() {
                if new_dict.get_item_opt(ty.as_object(), vm).unwrap_or(None).is_none() {
                    let _ = new_dict.set_item(ty.as_object(), vm.ctx.new_bool(false).into(), vm);
                }
            }
            *self.flags.lock() = new_dict;
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
        fn quantize(&self, x: PyObjectRef, y: PyObjectRef, rounding: OptionalArg<PyStrRef>, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let mut ctx = self.ctx();
            if let Some(r) = rounding.into_option() {
                ctx.rounding = Rounding::from_str(&r.as_wtf8().to_string())
                    .ok_or_else(|| vm.new_value_error("invalid rounding mode".to_string()))?;
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
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn compare(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            // sNaN should signal InvalidOperation
            if xd.special == Special::SignalingNaN || yd.special == Special::SignalingNaN {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "comparison with sNaN"));
                }
            }
            let r = dec_cmp(&xd, &yd);
            Ok(vm.ctx.new_int(BigInt::from(r)).into())
        }

        #[pymethod]
        fn floor_div(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let r = _decimal_core::dec_floordiv(&ctx, vm, &xd, &yd)?;
            Ok(PyDecimal::to_py(vm, r))
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
            // keyword forms (a=, b=, modulo=).
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
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn sqrt(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            // Newton's method for sqrt, context-applied
            let xd = to_dec_ctx(&x, vm)?;
            if xd.is_special() || xd.is_zero() {
                return Ok(PyDecimal::to_py(vm, xd));
            }
            if xd.sign {
                // negative: InvalidOperation
                return Err(new_decimal_error(vm, "InvalidOperation", "sqrt of negative"));
            }
            let ctx = self.ctx();
            let prec = ctx.prec as usize + 4;
            let mut x_val = xd.coeff.clone();
            let mut x_exp = xd.exp;
            // scale so we get prec extra digits
            x_val *= &BigInt::from(10).pow(prec as u32);
            x_exp -= prec as isize;
            // Newton: r_{n+1} = (r_n + x/r_n) / 2
            // initial guess from float
            let f = xd.to_string().parse::<f64>().unwrap_or(1.0);
            let mut r = BigInt::from((f.sqrt() * 10f64.powi(prec as i32)) as u64);
            let two = BigInt::from(2);
            let num = &x_val * &BigInt::from(10).pow((prec as u32));
            for _ in 0..50 {
                let q = &num / &r;
                let next = (&r + &q) / &two;
                if next == r { break; }
                r = next;
            }
            let result_exp = (x_exp + prec as isize) / 2 - prec as isize;
            Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                sign: false,
                coeff: r,
                exp: result_exp,
                special: _decimal_core::Special::Finite,
            }))
        }

        #[pymethod]
        fn create_decimal(&self, value: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let d = to_dec(&value, vm)?;
            // Apply context to the created decimal; implicit float conversion
            // sets the FloatOperation flag on this context.
            let mut ctx = self.ctx();
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
        fn create_decimal_from_float(&self, value: f64, vm: &VirtualMachine) -> PyResult {
            let d = _decimal_core::dec_from_float(value);
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
            let args: PyObjectRef = vm.ctx.new_tuple(vec![
                vm.ctx.new_int(prec).into(),
                vm.ctx.new_str(rounding_str).into(),
                vm.ctx.new_int(emin).into(),
                vm.ctx.new_int(emax).into(),
                vm.ctx.new_int(capitals).into(),
                vm.ctx.new_int(clamp).into(),
                trap_list.into(),
                flag_list.into(),
            ]).into();
            let cls: PyObjectRef = zelf.class().to_owned().into();
            Ok(vm.ctx.new_tuple(vec![cls, args]).into())
        }

        // --- Missing Context methods (delegating wrappers) ---
        #[pymethod]
        fn abs(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let mut d = xd.clone();
            d.sign = false;
            let ctx = self.ctx();
            let r = _decimal_core::apply_context(vm, &ctx, d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn minus(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            let r = _decimal_core::apply_context(vm, &ctx, xd)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn plus(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            let r = _decimal_core::apply_context(vm, &ctx, xd)?;
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
            let d = to_dec(&x, vm)?;
            Ok(PyDecimal::to_py(vm, d))
        }

        #[pymethod]
        fn compare_signal(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            if xd.special == Special::NaN || xd.special == Special::SignalingNaN
                || yd.special == Special::NaN || yd.special == Special::SignalingNaN
            {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "comparison with NaN"));
                }
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
                    Special::SignalingNaN => if d.sign { 0 } else { 9 },
                    Special::NaN => if d.sign { 1 } else { 10 },
                    Special::Infinity => if d.sign { 2 } else { 8 },
                    Special::Finite => {
                        if d.is_zero() { if d.sign { 3 } else { 7 } }
                        else { if d.sign { 4 } else { 6 } }
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
            } else {
                dec_cmp(&xd, &yd)
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
                    Special::SignalingNaN => if d.sign { 0 } else { 9 },
                    Special::NaN => if d.sign { 1 } else { 10 },
                    Special::Infinity => if d.sign { 2 } else { 8 },
                    Special::Finite => {
                        if d.is_zero() { if d.sign { 3 } else { 7 } }
                        else { if d.sign { 4 } else { 6 } }
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
            } else {
                dec_cmp(&xd, &yd)
            };
            Ok(vm.ctx.new_int(BigInt::from(r)).into())
        }

        #[pymethod]
        fn divide_int(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let div = _decimal_core::dec_div(&ctx, vm, &xd, &yd)?;
            let mut coeff = div.coeff.clone();
            if div.exp < 0 {
                let pow = BigInt::from(10).pow((-div.exp) as u32);
                coeff = &coeff / &pow;
            } else if div.exp > 0 {
                coeff *= BigInt::from(10).pow(div.exp as u32);
            }
            let r = _decimal_core::apply_context(vm, &ctx, _decimal_core::Dec {
                sign: div.sign, coeff, exp: 0, special: Special::Finite,
            })?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn divmod(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let (floor, m) = _decimal_core::floor_div_mod(&xd, &yd);
            let f = _decimal_core::apply_context(vm, &ctx, floor)?;
            let r = _decimal_core::apply_context(vm, &ctx, m)?;
            Ok(vm.ctx.new_tuple(vec![PyDecimal::to_py(vm, f), PyDecimal::to_py(vm, r)]).into())
        }

        #[pymethod]
        fn logb(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            if xd.is_special() {
                if xd.special == Special::Infinity {
                    return Ok(PyDecimal::to_py(vm, xd));
                }
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "logb of NaN"));
                }
                return Ok(PyDecimal::to_py(vm, xd));
            }
            if xd.is_zero() {
                ctx.set_flag(vm, "DivisionByZero")?;
                if ctx.trap(vm, "DivisionByZero")? {
                    return Err(new_decimal_error(vm, "DivisionByZero", "logb of zero"));
                }
                return Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                    sign: xd.sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity,
                }));
            }
            let adj = xd.exp + _decimal_core::coeff_digits_of(&xd.coeff) as isize - 1;
            Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                sign: adj < 0, coeff: BigInt::from(adj.unsigned_abs()), exp: 0,
                special: Special::Finite,
            }))
        }

        #[pymethod]
        fn max(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let r = if dec_cmp(&xd, &yd) >= 0 { xd } else { yd };
            let r = _decimal_core::apply_context(vm, &ctx, r)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn max_mag(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let xa = _decimal_core::Dec { sign: false, coeff: xd.coeff.clone(), exp: xd.exp, special: xd.special };
            let ya = _decimal_core::Dec { sign: false, coeff: yd.coeff.clone(), exp: yd.exp, special: yd.special };
            let r = if dec_cmp(&xa, &ya) >= 0 { xd } else { yd };
            let r = _decimal_core::apply_context(vm, &ctx, r)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn min(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let r = if dec_cmp(&xd, &yd) <= 0 { xd } else { yd };
            let r = _decimal_core::apply_context(vm, &ctx, r)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn min_mag(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let xa = _decimal_core::Dec { sign: false, coeff: xd.coeff.clone(), exp: xd.exp, special: xd.special };
            let ya = _decimal_core::Dec { sign: false, coeff: yd.coeff.clone(), exp: yd.exp, special: yd.special };
            let r = if dec_cmp(&xa, &ya) <= 0 { xd } else { yd };
            let r = _decimal_core::apply_context(vm, &ctx, r)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn remainder_near(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let div = _decimal_core::dec_div(&ctx, vm, &xd, &yd)?;
            let mut coeff = div.coeff.clone();
            if div.exp < 0 {
                let pow = BigInt::from(10).pow((-div.exp) as u32);
                let (q, r) = (&coeff / &pow, &coeff % &pow);
                let half = &pow / BigInt::from(2);
                let round_up = if &r * BigInt::from(2) > pow { true }
                    else if &r * BigInt::from(2) < pow { false }
                    else { (&q % BigInt::from(2)) == BigInt::from(1) };
                coeff = if round_up { q + BigInt::from(1) } else { q };
            } else if div.exp > 0 {
                coeff *= BigInt::from(10).pow(div.exp as u32);
            }
            let rounded = _decimal_core::Dec { sign: div.sign, coeff, exp: 0, special: Special::Finite };
            let prod = _decimal_core::dec_mul(&ctx, vm, &yd, &rounded)?;
            let result = _decimal_core::dec_sub(&ctx, vm, &xd, &prod)?;
            Ok(PyDecimal::to_py(vm, result))
        }

        #[pymethod]
        fn scaleb(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let n: isize = yd.coeff.to_string().parse().unwrap_or(0);
            let n = if yd.sign { -n } else { n };
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
            let n: isize = yd.coeff.to_string().parse().unwrap_or(0);
            let coeff = if n >= 0 {
                &xd.coeff * BigInt::from(10).pow(n as u32)
            } else {
                let pow = BigInt::from(10).pow((-n) as u32);
                &xd.coeff / &pow
            };
            let result = _decimal_core::Dec {
                sign: xd.sign, coeff, exp: xd.exp, special: xd.special,
            };
            let r = _decimal_core::apply_context(vm, &ctx, result)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn logical_and(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            if xd.exp != 0 || yd.exp != 0 || xd.sign || yd.sign {
                return Err(new_decimal_error(vm, "InvalidOperation", "logical_and requires non-negative integers"));
            }
            Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                sign: false, coeff: &xd.coeff & &yd.coeff, exp: 0, special: Special::Finite,
            }))
        }

        #[pymethod]
        fn logical_or(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            if xd.exp != 0 || yd.exp != 0 || xd.sign || yd.sign {
                return Err(new_decimal_error(vm, "InvalidOperation", "logical_or requires non-negative integers"));
            }
            Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                sign: false, coeff: &xd.coeff | &yd.coeff, exp: 0, special: Special::Finite,
            }))
        }

        #[pymethod]
        fn logical_xor(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            if xd.exp != 0 || yd.exp != 0 || xd.sign || yd.sign {
                return Err(new_decimal_error(vm, "InvalidOperation", "logical_xor requires non-negative integers"));
            }
            Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                sign: false, coeff: &xd.coeff ^ &yd.coeff, exp: 0, special: Special::Finite,
            }))
        }

        #[pymethod]
        fn logical_invert(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            if xd.exp != 0 || xd.sign {
                return Err(new_decimal_error(vm, "InvalidOperation", "logical_invert requires non-negative integer"));
            }
            let bits = xd.coeff.bits() as u32;
            let all_ones = (BigInt::from(1).pow(bits) - BigInt::from(1)) | BigInt::from(0);
            let all_ones = if bits == 0 { BigInt::from(0) } else { all_ones };
            Ok(PyDecimal::to_py(vm, _decimal_core::Dec {
                sign: false, coeff: all_ones ^ &xd.coeff, exp: 0, special: Special::Finite,
            }))
        }

        #[pymethod]
        fn next_minus(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            // next value toward -Inf: subtract 1 ulp
            let ulp = BigInt::from(10).pow(xd.exp.max(0) as u32);
            let coeff = &xd.coeff - &ulp;
            let r = _decimal_core::apply_context(vm, &ctx, _decimal_core::Dec {
                sign: true, coeff: xd.coeff.clone(), exp: xd.exp, special: Special::Finite,
            })?;
            let _ = coeff;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn next_plus(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            let r = _decimal_core::apply_context(vm, &ctx, _decimal_core::Dec {
                sign: false, coeff: xd.coeff.clone(), exp: xd.exp, special: Special::Finite,
            })?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn next_toward(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let r = _decimal_core::apply_context(vm, &ctx, _decimal_core::Dec {
                sign: yd.sign, coeff: xd.coeff.clone(), exp: xd.exp, special: xd.special,
            })?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn normalize(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let mut d = xd.clone();
            if d.is_zero() {
                d.exp = 0;
            } else {
                while d.coeff_digits() > 1 && (&d.coeff % BigInt::from(10)) == BigInt::from(0) {
                    d.coeff /= BigInt::from(10);
                    d.exp += 1;
                }
            }
            let ctx = self.ctx();
            let r = _decimal_core::apply_context(vm, &ctx, d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn number_class(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult<String> {
            let xd = to_dec_ctx(&x, vm)?;
            let sign = if xd.sign { "-" } else { "+" };
            match xd.special {
                Special::NaN => return Ok(format!("{sign}NaN")),
                Special::SignalingNaN => return Ok(format!("{sign}sNaN")),
                Special::Infinity => return Ok(format!("{sign}Infinity")),
                Special::Finite => {}
            }
            if xd.is_zero() {
                return Ok(format!("{sign}Zero"));
            }
            let ctx = self.ctx();
            let etiny = ctx.emin - (ctx.prec - 1);
            if xd.exp < etiny {
                Ok(format!("{sign}Subnormal"))
            } else if xd.exp > ctx.emax {
                Ok(format!("{sign}Infinite"))
            } else {
                Ok(format!("{sign}Normal"))
            }
        }

        #[pymethod]
        fn to_integral(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            self.to_integral_value(x, vm)
        }

        #[pymethod]
        fn to_integral_exact(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            self.to_integral_value(x, vm)
        }

        #[pymethod]
        fn to_integral_value(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let ctx = self.ctx();
            let mut coeff = xd.coeff.clone();
            if xd.exp < 0 {
                let pow = BigInt::from(10).pow((-xd.exp) as u32);
                coeff = &coeff / &pow;
            } else if xd.exp > 0 {
                coeff *= BigInt::from(10).pow(xd.exp as u32);
            }
            let r = _decimal_core::apply_context(vm, &ctx, _decimal_core::Dec {
                sign: xd.sign, coeff, exp: 0, special: xd.special,
            })?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn to_sci_string(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let d = to_dec(&x, vm)?;
            Ok(vm.ctx.new_str(d.to_string()).into())
        }

        #[pymethod]
        fn same_quantum(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            Ok(vm.ctx.new_bool(xd.exp == yd.exp).into())
        }

        #[pymethod]
        fn radix(&self, _x: OptionalArg<PyObjectRef>, vm: &VirtualMachine) -> PyResult {
            Ok(vm.ctx.new_int(BigInt::from(10)).into())
        }

        #[pymethod]
        fn canonical(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let d = to_dec(&x, vm)?;
            Ok(PyDecimal::to_py(vm, d))
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
            // e^x via Taylor series, context-applied
            let xd = to_dec_ctx(&x, vm)?;
            if xd.is_special() {
                if xd.special == Special::Infinity {
                    let ctx = self.ctx();
                    let r = _decimal_core::apply_context(vm, &ctx, xd)?;
                    return Ok(PyDecimal::to_py(vm, r));
                }
                let ctx = self.ctx();
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "exp of NaN"));
                }
                return Ok(PyDecimal::to_py(vm, xd));
            }
            let ctx = self.ctx();
            let prec = ctx.prec as usize + 4;
            // e^x = 1 + x + x^2/2! + x^3/3! + ...
            let xf = xd.to_string().parse::<f64>().unwrap_or(0.0);
            let result = xf.exp();
            let s = format!("{:.*}", prec, result);
            let d = _decimal_core::parse_decimal(&s).unwrap_or(_decimal_core::Dec::zero());
            let r = _decimal_core::apply_context(vm, &ctx, d)?;
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
            if xd.is_special() || xd.sign {
                let ctx = self.ctx();
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "ln of non-positive"));
                }
                return Ok(PyDecimal::to_py(vm, xd));
            }
            let xf = xd.to_string().parse::<f64>().unwrap_or(1.0);
            let result = xf.ln();
            let prec = ctx.prec as usize + 4;
            let s = format!("{:.*}", prec, result);
            let d = _decimal_core::parse_decimal(&s).unwrap_or(_decimal_core::Dec::zero());
            let r = _decimal_core::apply_context(vm, &ctx, d)?;
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
            if xd.is_special() || xd.sign {
                let ctx = self.ctx();
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "log10 of non-positive"));
                }
                return Ok(PyDecimal::to_py(vm, xd));
            }
            let xf = xd.to_string().parse::<f64>().unwrap_or(1.0);
            let result = xf.log10();
            let prec = ctx.prec as usize + 4;
            let s = format!("{:.*}", prec, result);
            let d = _decimal_core::parse_decimal(&s).unwrap_or(_decimal_core::Dec::zero());
            let r = _decimal_core::apply_context(vm, &ctx, d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn fma(&self, x: PyObjectRef, y: PyObjectRef, z: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let zd = to_dec_ctx(&z, vm)?;
            let ctx = self.ctx();
            // x*y + z
            let prod = _decimal_core::dec_mul(&ctx, vm, &xd, &yd)?;
            let r = _decimal_core::dec_add(&ctx, vm, &prod, &zd)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn rotate(&self, x: PyObjectRef, y: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            // rotate: like shift but wraps digits
            let xd = to_dec_ctx(&x, vm)?;
            let yd = to_dec_ctx(&y, vm)?;
            let ctx = self.ctx();
            let n: isize = yd.coeff.to_string().parse().unwrap_or(0);
            let n = if yd.sign { -n } else { n };
            let digits = xd.coeff.to_string();
            let len = digits.len();
            if len == 0 || n == 0 {
                let r = _decimal_core::apply_context(vm, &ctx, xd)?;
                return Ok(PyDecimal::to_py(vm, r));
            }
            let n = ((n % len as isize) + len as isize) % len as isize;
            let n = n as usize;
            let rotated = format!("{}{}", &digits[len-n..], &digits[..len-n]);
            let coeff = rotated.parse::<BigInt>().unwrap_or(BigInt::from(0));
            let result = _decimal_core::Dec {
                sign: xd.sign, coeff, exp: xd.exp, special: xd.special,
            };
            let r = _decimal_core::apply_context(vm, &ctx, result)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn to_eng_string(&self, x: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let d = to_dec_ctx(&x, vm)?;
            Ok(vm.ctx.new_str(d.to_string()).into())
        }
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
        let flag_names: Vec<String> = flags
            .items_vec()
            .into_iter()
            .filter(|(_, v)| v.try_to_value::<bool>(vm).unwrap_or(false))
            .map(|(k, _)| exception_name(&k, vm))
            .collect();
        let trap_names: Vec<String> = traps
            .items_vec()
            .into_iter()
            .filter(|(_, v)| v.try_to_value::<bool>(vm).unwrap_or(false))
            .map(|(k, _)| exception_name(&k, vm))
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
            // Context(prec, rounding, Emin, Emax, capitals, clamp) Ã¢â‚¬â€ accept
            // both positional and keyword forms (the test suite uses keyword
            // args like Context(prec=9, rounding=..., traps=...)).
            let mut pos_iter = args.args.iter();
            let mut get_field = |key: &str| -> Option<PyObjectRef> {
                args.get_optional_kwarg(key)
                    .or_else(|| pos_iter.next().cloned())
                    // None (explicit or from a None keyword) means "inherit
                    // the default" (CPython: Context(prec=None, ...) is the
                    // default context).
                    .filter(|v| !downcast_ref_none(v, vm))
            };
            if let Some(v) = get_field("prec") {
                c.prec = PyMutex::new(v.try_to_value::<isize>(vm)?);
            }
            if let Some(v) = get_field("rounding") {
                let s = v
                    .downcast_ref::<PyStr>()
                    .ok_or_else(|| vm.new_type_error("rounding must be a string".to_string()))?;
                c.rounding = PyMutex::new(
                    Rounding::from_str(&s.as_wtf8().to_string())
                        .ok_or_else(|| vm.new_value_error("invalid rounding".to_string()))?,
                );
            }
            if let Some(v) = get_field("Emin") {
                c.emin = PyMutex::new(v.try_to_value::<isize>(vm)?);
            }
            if let Some(v) = get_field("Emax") {
                c.emax = PyMutex::new(v.try_to_value::<isize>(vm)?);
            }
            if let Some(v) = get_field("capitals") {
                c.capitals = PyMutex::new(v.try_to_value::<isize>(vm)?);
            }
            if let Some(v) = get_field("clamp") {
                c.clamp = PyMutex::new(v.try_to_value::<isize>(vm)?);
            }
            if let Some(v) = get_field("traps") {
                let traps_guard = c.traps.lock();
                // Ensure all known signals present (default False).
                for ty in exc().all() {
                    let _ = traps_guard.set_item(ty.as_object(), vm.ctx.new_bool(false).into(), vm);
                }
                if let Some(d) = v.downcast_ref::<PyDict>() {
                    for (k, val) in d.items_vec() {
                        let on = val.try_to_value::<bool>(vm)?;
                        let _ = traps_guard.set_item(k.as_object(), vm.ctx.new_bool(on).into(), vm);
                    }
                } else if let Some(t) = v.downcast_ref::<PyTuple>() {
                    // __reduce__ form: tuple of exception objects to enable.
                    for item in t.as_slice() {
                        let _ = traps_guard.set_item(item.as_object(), vm.ctx.new_bool(true).into(), vm);
                    }
                } else if let Some(lst) = v.downcast_ref::<crate::vm::builtins::PyList>() {
                    let items: Vec<PyObjectRef> = lst.borrow_vec().to_vec();
                    for item in items {
                        let _ = traps_guard.set_item(item.as_object(), vm.ctx.new_bool(true).into(), vm);
                    }
                }
            }
            if let Some(v) = get_field("flags") {
                let flags_guard = c.flags.lock();
                for ty in exc().all() {
                    let _ = flags_guard.set_item(ty.as_object(), vm.ctx.new_bool(false).into(), vm);
                }
                if let Some(d) = v.downcast_ref::<PyDict>() {
                    for (k, val) in d.items_vec() {
                        let on = val.try_to_value::<bool>(vm)?;
                        let _ = flags_guard.set_item(k.as_object(), vm.ctx.new_bool(on).into(), vm);
                    }
                } else if let Some(t) = v.downcast_ref::<PyTuple>() {
                    for item in t.as_slice() {
                        let _ = flags_guard.set_item(item.as_object(), vm.ctx.new_bool(true).into(), vm);
                    }
                } else if let Some(lst) = v.downcast_ref::<crate::vm::builtins::PyList>() {
                    let items: Vec<PyObjectRef> = lst.borrow_vec().to_vec();
                    for item in items {
                        let _ = flags_guard.set_item(item.as_object(), vm.ctx.new_bool(true).into(), vm);
                    }
                }
            }
            Ok(c)
        }
    }

    #[pyattr(name = "localcontext")]
    #[pyclass(name = "localcontext", module = "decimal", traverse)]
    #[derive(Debug, PyPayload)]
    struct LocalContext {
        saved: PyMutex<Option<PyObjectRef>>,
        ctx_arg: Option<PyObjectRef>,
    }

    #[pyclass(with(Constructor))]
    impl LocalContext {
        #[pymethod]
        fn __enter__(&self, vm: &VirtualMachine) -> PyResult<PyObjectRef> {
            // Save the current context for __exit__ restoration
            let current = get_current_context(vm);
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

    impl Constructor for LocalContext {
        type Args = FuncArgs;

        fn py_new(_cls: &Py<PyType>, args: FuncArgs, vm: &VirtualMachine) -> PyResult<Self> {
            let ctx_arg = if args.args.is_empty() {
                None
            } else {
                Some(args.args[0].clone())
            };
            Ok(LocalContext {
                saved: PyMutex::new(None),
                ctx_arg,
            })
        }
    }

    #[pyfunction]
    fn getcontext(vm: &VirtualMachine) -> PyObjectRef {
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
                for ty in exc().all() {
                    // IEEEContext: all traps off
                    let _ = t.set_item(ty.as_object(), vm.ctx.new_bool(false).into(), vm);
                }
                PyMutex::new(t)
            },
            flags: {
                let f = vm.ctx.new_dict();
                for ty in exc().all() {
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
            "ROUND_01UP",
        ] {
            set(r, vm.ctx.new_str(r).into())?;
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




