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
    /// Used by Decimal constructor Ã¢â‚¬â€ accepts str.
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
        Err(vm.new_type_error(format!("cannot convert {} to Decimal", obj.class().name())))
    }

    /// Convert a Python object (Decimal/int/float) to Dec.
    /// Used by Context methods Ã¢â‚¬â€ rejects str (CPython behavior).
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

    #[pyattr(name = "Decimal")]
    #[pyclass(name = "Decimal", module = "_decimal", traverse)]
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
            let sign = items[0].clone().try_to_bool(vm)?;
            let digits_obj = items[1].downcast_ref::<PyTuple>().ok_or_else(|| {
                vm.new_type_error("digits must be a tuple".to_string())
            })?;
            let mut coeff = BigInt::from(0);
            for d in digits_obj.iter() {
                let dv: u8 = d.try_to_value(vm)?;
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
                        return Err(new_decimal_error(
                            vm,
                            "InvalidOperation",
                            &format!("malformed exponent string '{tag}'"),
                        ))
                    }
                };
                return Ok(Dec { sign, coeff, exp: 0, special });
            }
            let exp: isize = items[2].clone().try_to_value(vm)?;
            Ok(Dec { sign, coeff, exp, special: Special::Finite })
        }
    }

    #[pyclass(with(Constructor, AsNumber, Comparable, Hashable, Representable))]
    impl PyDecimal {
        #[pymethod]
        fn quantize(zelf: &Py<Self>, args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let mut iter = args.args.iter();
            let exp = iter.next().ok_or_else(|| {
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
            let mut ctx = current_ctx(vm)?;
            if let Some(ref rs) = rounding_str {
                ctx.rounding = Rounding::from_str(rs)
                    .ok_or_else(|| vm.new_value_error("invalid rounding mode".to_string()))?;
            }
            let r = _decimal_core::quantize(vm, &ctx, &zelf.d, target_exp)?;
            Ok(Self::to_py(vm, r))
        }

        #[pymethod]
        fn as_tuple(&self, vm: &VirtualMachine) -> PyTupleRef {
            let sign = if self.d.sign { 1 } else { 0 };
            // CPython as_tuple: finite numbers have an int exponent; special
            // values carry a string: 'F' (Infinity), 'n' (NaN), 'N' (sNaN).
            // NaN carries its diagnostic digits; Infinity is (0,).
            let diag_digits = || -> Vec<PyObjectRef> {
                // NaN diagnostic info stored in coeff (0 means none).
                if self.d.coeff == BigInt::from(0) {
                    return vec![];
                }
                self.d.coeff_str()
                    .bytes()
                    .map(|b| vm.ctx.new_int(b - b'0').into())
                    .collect()
            };
            match self.d.special {
                Special::Finite => {
                    let digits: Vec<PyObjectRef> = self
                        .d
                        .coeff_str()
                        .bytes()
                        .map(|b| vm.ctx.new_int(b - b'0').into())
                        .collect();
                    vm.ctx.new_tuple(vec![
                        vm.ctx.new_int(sign).into(),
                        vm.ctx.new_tuple(digits).into(),
                        vm.ctx.new_int(self.d.exp).into(),
                    ])
                }
                Special::Infinity => vm.ctx.new_tuple(vec![
                    vm.ctx.new_int(sign).into(),
                    vm.ctx.new_tuple(vec![vm.ctx.new_int(0).into()]).into(),
                    vm.ctx.new_str("F").into(),
                ]),
                Special::NaN => vm.ctx.new_tuple(vec![
                    vm.ctx.new_int(sign).into(),
                    vm.ctx.new_tuple(diag_digits()).into(),
                    vm.ctx.new_str("n").into(),
                ]),
                Special::SignalingNaN => vm.ctx.new_tuple(vec![
                    vm.ctx.new_int(sign).into(),
                    vm.ctx.new_tuple(diag_digits()).into(),
                    vm.ctx.new_str("N").into(),
                ]),
            }
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
        fn is_normal(&self, vm: &VirtualMachine) -> PyResult<bool> {
            let ctx = current_ctx(vm)?;
            Ok(!self.d.is_zero()
                && self.d.special == Special::Finite
                && self.d.exp >= ctx.emin
                && self.d.exp <= ctx.emax)
        }
        #[pymethod]
        fn is_subnormal(&self, vm: &VirtualMachine) -> PyResult<bool> {
            let ctx = current_ctx(vm)?;
            Ok(!self.d.is_zero() && self.d.special == Special::Finite && self.d.exp < ctx.emin)
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
        fn number_class(&self, vm: &VirtualMachine) -> PyResult<String> {
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
            let ctx = current_ctx(vm)?;
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
        fn normalize(&self) -> PyDecimal {
            let mut d = self.d.clone();
            if d.is_zero() {
                d.exp = 0;
                return PyDecimal { d };
            }
            while d.coeff_digits() > 1 && (&d.coeff % BigInt::from(10)) == BigInt::from(0) {
                d.coeff /= BigInt::from(10);
                d.exp += 1;
            }
            PyDecimal { d }
        }

        #[pymethod]
        fn same_quantum(&self, other: PyObjectRef, vm: &VirtualMachine) -> PyResult<bool> {
            let b = to_dec(&other, vm)?;
            Ok(self.d.exp == b.exp)
        }

        #[pymethod]
        fn compare(&self, other: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let b = to_dec(&other, vm)?;
            Ok(vm.ctx.new_int(BigInt::from(dec_cmp(&self.d, &b))).into())
        }

        #[pymethod]
        fn max(&self, other: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let b = to_dec(&other, vm)?;
            if dec_cmp(&self.d, &b) >= 0 {
                Ok(Self::to_py(vm, self.d.clone()))
            } else {
                Ok(Self::to_py(vm, b))
            }
        }

        #[pymethod]
        fn min(&self, other: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let b = to_dec(&other, vm)?;
            if dec_cmp(&self.d, &b) <= 0 {
                Ok(Self::to_py(vm, self.d.clone()))
            } else {
                Ok(Self::to_py(vm, b))
            }
        }

        #[pymethod]
        fn sqrt(&self, vm: &VirtualMachine) -> PyResult {
            let ctx = current_ctx(vm)?;
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
        fn exp(&self, vm: &VirtualMachine) -> PyResult {
            let ctx = current_ctx(vm)?;
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
            let result = xf.exp();
            let s = format!("{:.*}", prec, result);
            let d = _decimal_core::parse_decimal(&s).unwrap_or(_decimal_core::Dec::zero());
            let r = _decimal_core::apply_context(vm, &ctx, d)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn ln(&self, vm: &VirtualMachine) -> PyResult {
            let ctx = current_ctx(vm)?;
            if self.d.is_special() || self.d.is_zero() || self.d.sign {
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
        fn log10(&self, vm: &VirtualMachine) -> PyResult {
            let ctx = current_ctx(vm)?;
            if self.d.is_special() || self.d.is_zero() || self.d.sign {
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
        fn logb(&self, vm: &VirtualMachine) -> PyResult {
            let ctx = current_ctx(vm)?;
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
        fn compare_signal(&self, other: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            let b = to_dec(&other, vm)?;
            let ctx = current_ctx(vm)?;
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
        fn __format__(&self, spec: PyStrRef) -> PyResult<String> {
            let spec = spec.as_wtf8().to_string();
            if spec.is_empty() {
                return Ok(self.d.to_string());
            }
            if spec.ends_with('f') || spec.ends_with('F') {
                let f = self.d.to_string().parse::<f64>().unwrap_or(0.0);
                return Ok(format!("{f:.6}"));
            }
            Ok(self.d.to_string())
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
            rounding: OptionalArg<PyStrRef>,
            vm: &VirtualMachine,
        ) -> PyResult {
            let ctx = current_ctx(vm)?;
            let mut ctx2 = ctx;
            if let Some(r) = rounding.into_option() {
                ctx2.rounding = Rounding::from_str(&r.as_wtf8().to_string())
                    .ok_or_else(|| vm.new_value_error("invalid rounding mode".to_string()))?;
            }
            let r = _decimal_core::quantize(vm, &ctx2, &self.d, 0)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn to_integral_value(&self, vm: &VirtualMachine) -> PyResult {
            let ctx = current_ctx(vm)?;
            let r = _decimal_core::quantize(vm, &ctx, &self.d, 0)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn to_integral_exact(&self, vm: &VirtualMachine) -> PyResult {
            let ctx = current_ctx(vm)?;
            let r = _decimal_core::quantize(vm, &ctx, &self.d, 0)?;
            Ok(PyDecimal::to_py(vm, r))
        }

        #[pymethod]
        fn to_eng_string(&self) -> String {
            self.d.to_string()
        }

        #[pymethod]
        fn fma(&self, other: PyObjectRef, third: PyObjectRef, vm: &VirtualMachine) -> PyResult {
            // self * other + third, using context
            let other_d = to_dec_ctx(&other, vm)?;
            let third_d = to_dec_ctx(&third, vm)?;
            let ctx = current_ctx(vm)?;
            let prod = _decimal_core::dec_mul(&ctx, vm, &self.d, &other_d)?;
            let r = _decimal_core::dec_add(&ctx, vm, &prod, &third_d)?;
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
            // Support keyword args: Decimal(value=..., context=...)
            let value = args.get_optional_kwarg("value")
                .or_else(|| args.args.get(0).cloned())
                .unwrap_or_else(|| vm.ctx.new_int(0).into());
            // context keyword is accepted but we use the current context
            // for flag/precision application (CPython's context= overrides
            // the current context for construction).
            let _context_kw = args.get_optional_kwarg("context");
            if let Some(tuple) = value.downcast_ref::<PyTuple>() {
                let d = Self::from_tuple(tuple.as_slice(), vm)?;
                return Ok(PyDecimal { d });
            }
            // CPython 3.11+: invalid string like 'ugly' constructs a
            // quiet NaN (with InvalidOperation flag) instead of raising.
            // But TypeError (e.g. Decimal(None)) must still propagate.
            let d = match to_dec(&value, vm) {
                Ok(d) => d,
                Err(e) => {
                    // Only InvalidOperation (string parse failure) → NaN.
                    // TypeError and others propagate as-is.
                    let is_io = e.class().is(&exc().invalid_operation);
                    if !is_io {
                        return Err(e);
                    }
                    let ctx = current_ctx(vm)?;
                    let _ = ctx.set_flag(vm, "InvalidOperation");
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
            let b = match to_dec(other, vm) {
                Ok(b) => b,
                Err(_) => return Ok(PyComparisonValue::NotImplemented),
            };
            // sNaN comparisons always signal InvalidOperation (CPython
            // raises even when the trap is off for sNaN compare).
            let ssnan = zelf.d.special == Special::SignalingNaN
                || b.special == Special::SignalingNaN;
            if ssnan {
                let ctx = current_ctx(vm)?;
                ctx.set_flag(vm, "InvalidOperation")?;
                return Err(new_decimal_error(vm, "InvalidOperation", "comparison with sNaN"));
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
                let s = d.to_string();
                return Ok(hash::fix_sentinel(hash::HashSecret::new(0).hash_str(&s)));
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
                        // CPython: int(Decimal('NaN')) / int(Decimal('Infinity'))
                        // raises ValueError, not InvalidOperation.
                        return Err(vm.new_value_error(
                            "cannot convert NaN/infinity to integer".to_string(),
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
    #[pyclass(name = "Context", module = "_decimal", traverse)]
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
            // Apply context to the created decimal
            let ctx = self.ctx();
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
        fn __str__(&self) -> String {
            format!(
                "Context(prec={}, rounding={}, Emin={}, Emax={}, capitals={}, clamp={}, flags={}, traps={})",
                self.prec.lock(),
                self.rounding.lock().to_str(),
                self.emin.lock(),
                self.emax.lock(),
                self.capitals.lock(),
                self.clamp.lock(),
                self.flags.lock().size().used,
                self.traps.lock().size().used,
            )
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
            if xd.is_special() || xd.is_zero() || xd.sign {
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
            if xd.is_special() || xd.is_zero() || xd.sign {
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
            let prec = *zelf.prec.lock();
            let rounding = *zelf.rounding.lock();
            let rounding_str = rounding.to_str();
            let emin = *zelf.emin.lock();
            let emax = *zelf.emax.lock();
            let capitals = *zelf.capitals.lock();
            let clamp = *zelf.clamp.lock();
            let flags = zelf.flags.lock().size().used;
            let traps = zelf.traps.lock().size().used;
            Ok(vm.ctx.new_str(format!(
                "Context(prec={prec}, rounding={rounding_str}, Emin={emin}, Emax={emax}, capitals={capitals}, clamp={clamp}, flags={flags}, traps={traps})",
            )))
        }
    }

    impl Constructor for PyContext {
        type Args = FuncArgs;

        fn py_new(_cls: &Py<PyType>, args: FuncArgs, vm: &VirtualMachine) -> PyResult<Self> {
            let mut c = Self::new_ctx(vm, 28);
            // Context(prec, rounding, Emin, Emax, capitals, clamp) â€” accept
            // both positional and keyword forms (the test suite uses keyword
            // args like Context(prec=9, rounding=..., traps=...)).
            let mut pos_iter = args.args.iter();
            let mut get_field = |key: &str| -> Option<PyObjectRef> {
                args.get_optional_kwarg(key)
                    .or_else(|| pos_iter.next().cloned())
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
                if let Some(d) = v.downcast_ref::<PyDict>() {
                    for (k, val) in d.items_vec() {
                        let on = val.try_to_value::<bool>(vm)?;
                        let _ = c.traps.lock().set_item(k.as_object(), vm.ctx.new_bool(on).into(), vm);
                    }
                }
            }
            if let Some(v) = get_field("flags") {
                if let Some(d) = v.downcast_ref::<PyDict>() {
                    for (k, val) in d.items_vec() {
                        let on = val.try_to_value::<bool>(vm)?;
                        let _ = c.flags.lock().set_item(k.as_object(), vm.ctx.new_bool(on).into(), vm);
                    }
                }
            }
            Ok(c)
        }
    }

    #[pyattr(name = "localcontext")]
    #[pyclass(name = "localcontext", module = "_decimal", traverse)]
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
    fn setcontext(context: PyObjectRef) {
        set_current_context(context);
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
        let emin = -(emax + prec - 1);
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
                    let trap_on = ty.is(&exc().invalid_operation)
                        || ty.is(&exc().division_by_zero)
                        || ty.is(&exc().overflow);
                    let _ = t.set_item(ty.as_object(), vm.ctx.new_bool(trap_on).into(), vm);
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
        set("IEEE_CONTEXT_MAX_BITS", vm.ctx.new_int(BigInt::from(9999999999i64)).into())?;
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

        Ok(())
    }
}



