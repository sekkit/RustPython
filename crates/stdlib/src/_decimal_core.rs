// spell-checker: ignore mpdecimal snan qnan etiny

use malachite_bigint::{BigInt, Sign};
use num_traits::Signed;

use crate::common::static_cell::StaticCell;
use rustpython_common::static_cell;
use crate::vm::{
    AsObject, PyObjectRef, PyResult, VirtualMachine,
    builtins::{PyBaseException, PyBaseExceptionRef, PyDictRef, PyFloat, PyInt, PyStrRef, PyTypeRef},
    types::Callable,
};

/// Special value kind of a Decimal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Special {
    Finite,
    Infinity,
    NaN,
    SignalingNaN,
}

/// Core decimal value: (-1)^sign * coeff * 10^exp
#[derive(Clone, Debug)]
pub(crate) struct Dec {
    pub sign: bool,
    pub coeff: BigInt, // non-negative
    pub exp: isize,
    pub special: Special,
}

impl Dec {
    pub fn zero() -> Self {
        Self { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Finite }
    }
    pub fn is_zero(&self) -> bool {
        self.special == Special::Finite && self.coeff == BigInt::from(0)
    }
    pub fn is_special(&self) -> bool {
        self.special != Special::Finite
    }
    /// Number of decimal digits of the (non-negative) coefficient.
    pub fn coeff_digits(&self) -> usize {
        let mut s = self.coeff.to_string();
        if s == "0" {
            return 1;
        }
        s = s.trim_start_matches('0').to_string();
        if s.is_empty() { 1 } else { s.len() }
    }
    /// digit string of coefficient without sign (no leading zeros except "0")
    pub fn coeff_str(&self) -> String {
        let s = self.coeff.to_string();
        if s == "0" { return "0".to_string(); }
        let t = s.trim_start_matches('0');
        if t.is_empty() { "0".to_string() } else { t.to_string() }
    }
    fn set_sign(mut self, neg: bool) -> Self {
        self.sign = neg;
        self
    }

    /// CPython/libmpdec-compatible string representation (__str__/__repr__ core).
    pub fn to_string(&self) -> String {
        let prefix = if self.sign { "-" } else { "" };
        match self.special {
            Special::Infinity => return format!("{prefix}Infinity"),
            // CPython displays the diagnostic payload digits for
            // NaN/sNaN: Decimal('NaN123') -> 'NaN123' (no payload for 0).
            Special::NaN => {
                if self.coeff == BigInt::from(0) {
                    return format!("{prefix}NaN");
                }
                return format!("{prefix}NaN{}", self.coeff_str());
            }
            Special::SignalingNaN => {
                if self.coeff == BigInt::from(0) {
                    return format!("{prefix}sNaN");
                }
                return format!("{prefix}sNaN{}", self.coeff_str());
            }
            Special::Finite => {}
        }
        let d = self.coeff_str();
        let e = self.exp;
        if e == 0 {
            format!("{prefix}{d}")
        } else if e > 0 {
            // scientific, normalized: d.dddE+exp
            let exp_show = e + d.len() as isize - 1;
            if d.len() == 1 {
                format!("{prefix}{d}E+{exp_show}")
            } else {
                format!("{prefix}{}.{}E+{exp_show}", &d[..1], &d[1..])
            }
        } else {
            // e < 0
            let ne = (-e) as usize;
            if ne <= d.len() {
                let cut = d.len() - ne;
                if cut == 0 {
                    format!("{prefix}0.{d}")
                } else {
                    format!("{prefix}{}.{}", &d[..cut], &d[cut..])
                }
            } else if ne <= 6 {
                format!("{prefix}0.{}{d}", "0".repeat(ne - d.len()))
            } else if e + d.len() as isize - 1 >= -6 && e + d.len() as isize - 1 <= 6 {
                // CPython keeps a fixed-point form when the ADJUSTED
                // exponent is modest in magnitude, even with many fractional
                // digits (e.g. 0.0000372 = 372e-7, adjusted -5).
                format!("{prefix}0.{}{d}", "0".repeat(ne - d.len()))
            } else {
                let exp_show = e + d.len() as isize - 1;
                if d.len() == 1 {
                    format!("{prefix}{d}E{exp_show}")
                } else {
                    format!("{prefix}{}.{}E{exp_show}", &d[..1], &d[1..])
                }
            }
        }
    }

    /// Engineering notation (exponent is a multiple of 3). Mirrors CPython's
    /// `Decimal.__str__(eng=True)`.
    pub fn to_eng_string(&self) -> String {
        let prefix = if self.sign { "-" } else { "" };
        match self.special {
            Special::Infinity => return format!("{prefix}Infinity"),
            Special::NaN => {
                if self.coeff == BigInt::from(0) {
                    return format!("{prefix}NaN");
                }
                return format!("{prefix}NaN{}", self.coeff_str());
            }
            Special::SignalingNaN => {
                if self.coeff == BigInt::from(0) {
                    return format!("{prefix}sNaN");
                }
                return format!("{prefix}sNaN{}", self.coeff_str());
            }
            Special::Finite => {}
        }
        let d = self.coeff_str();
        let leftdigits = self.exp + d.len() as isize;
        // dotplace: digits of the coefficient left of the decimal point.
        let dotplace = if self.exp <= 0 && leftdigits > -6 {
            leftdigits
        } else if d == "0" {
            (leftdigits + 1).rem_euclid(3) - 1
        } else {
            (leftdigits - 1).rem_euclid(3) + 1
        };
        let (intpart, fracpart) = if dotplace <= 0 {
            ("0".to_string(), format!(".{}{d}", "0".repeat(-dotplace as usize)))
        } else if dotplace >= d.len() as isize {
            (format!("{d}{}", "0".repeat((dotplace - d.len() as isize) as usize)), String::new())
        } else {
            (d[..dotplace as usize].to_string(), format!(".{}", &d[dotplace as usize..]))
        };
        if leftdigits == dotplace {
            format!("{prefix}{intpart}{fracpart}")
        } else {
            let exp = leftdigits - dotplace;
            format!("{prefix}{intpart}{fracpart}E{exp:+}")
        }
    }
}

/// Rounding modes, matching CPython's ROUND_* constants.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Rounding {
    Ceiling,
    Down,
    Floor,
    HalfDown,
    HalfEven,
    HalfUp,
    Up,
    Half05Up,
    Half01Up,
}

impl Rounding {
    pub(crate) fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "ROUND_CEILING" => Self::Ceiling,
            "ROUND_DOWN" => Self::Down,
            "ROUND_FLOOR" => Self::Floor,
            "ROUND_HALF_DOWN" => Self::HalfDown,
            "ROUND_HALF_EVEN" => Self::HalfEven,
            "ROUND_HALF_UP" => Self::HalfUp,
            "ROUND_UP" => Self::Up,
            "ROUND_05UP" => Self::Half05Up,
            "ROUND_01UP" => Self::Half01Up,
            _ => return None,
        })
    }
    pub(crate) fn to_str(&self) -> &'static str {
        match self {
            Self::Ceiling => "ROUND_CEILING",
            Self::Down => "ROUND_DOWN",
            Self::Floor => "ROUND_FLOOR",
            Self::HalfDown => "ROUND_HALF_DOWN",
            Self::HalfEven => "ROUND_HALF_EVEN",
            Self::HalfUp => "ROUND_HALF_UP",
            Self::Up => "ROUND_UP",
            Self::Half05Up => "ROUND_05UP",
            Self::Half01Up => "ROUND_01UP",
        }
    }
}

/// Exception classes for the decimal module (built at module init).
pub(crate) struct DecExc {
    pub decimal_exception: PyTypeRef,
    pub clamped: PyTypeRef,
    pub invalid_operation: PyTypeRef,
    pub conversion_syntax: PyTypeRef,
    pub division_impossible: PyTypeRef,
    pub division_undefined: PyTypeRef,
    pub invalid_context: PyTypeRef,
    pub division_by_zero: PyTypeRef,
    pub inexact: PyTypeRef,
    pub overflow: PyTypeRef,
    pub underflow: PyTypeRef,
    pub rounded: PyTypeRef,
    pub subnormal: PyTypeRef,
    pub float_operation: PyTypeRef,
}

impl DecExc {
    pub(crate) fn all(&self) -> Vec<&PyTypeRef> {
        vec![
            &self.decimal_exception,
            &self.clamped,
            &self.invalid_operation,
            &self.conversion_syntax,
            &self.division_impossible,
            &self.division_undefined,
            &self.invalid_context,
            &self.division_by_zero,
            &self.inexact,
            &self.overflow,
            &self.underflow,
            &self.rounded,
            &self.subnormal,
            &self.float_operation,
        ]
    }

    /// Look up a signal exception type by its class name (e.g. "Clamped").
    /// Returns None for names that are not decimal signals.
    pub(crate) fn by_name(&self, name: &str) -> Option<&PyTypeRef> {
        match name {
            "DecimalException" => Some(&self.decimal_exception),
            "Clamped" => Some(&self.clamped),
            "InvalidOperation" => Some(&self.invalid_operation),
            "ConversionSyntax" => Some(&self.conversion_syntax),
            "DivisionImpossible" => Some(&self.division_impossible),
            "DivisionUndefined" => Some(&self.division_undefined),
            "InvalidContext" => Some(&self.invalid_context),
            "DivisionByZero" => Some(&self.division_by_zero),
            "Inexact" => Some(&self.inexact),
            "Overflow" => Some(&self.overflow),
            "Underflow" => Some(&self.underflow),
            "Rounded" => Some(&self.rounded),
            "Subnormal" => Some(&self.subnormal),
            "FloatOperation" => Some(&self.float_operation),
            _ => None,
        }
    }
}

static_cell! {
    static EXC: DecExc;
}

pub(crate) fn exc() -> &'static DecExc {
    EXC.get().expect("_decimal exceptions not initialized")
}

/// Whether an exception type is one of the 9 signals that may appear in a
/// Context's flags/traps dict (CPython SignalDict keys). DecimalException and
/// its non-signal subclasses (ConversionSyntax, DivisionImpossible, etc.) are
/// not signals.
pub(crate) fn is_signal(ty: &PyTypeRef) -> bool {
    let e = exc();
    ty.is(&e.clamped)
        || ty.is(&e.invalid_operation)
        || ty.is(&e.division_by_zero)
        || ty.is(&e.inexact)
        || ty.is(&e.float_operation)
        || ty.is(&e.overflow)
        || ty.is(&e.rounded)
        || ty.is(&e.subnormal)
        || ty.is(&e.underflow)
}

/// Build the exception hierarchy once per process.
pub(crate) fn init_exceptions(vm: &VirtualMachine) {
    let _ = EXC.get_or_init(|| {
        let arith = vm.ctx.exceptions.arithmetic_error.to_owned();
        let zero_div = vm.ctx.exceptions.zero_division_error.to_owned();
        let base = |name: &str, bases: Vec<PyTypeRef>| -> PyTypeRef {
            vm.ctx.new_exception_type("decimal", name, Some(bases))
        };
        let decimal_exception = base("DecimalException", vec![arith.clone()]);
        let clamped = base("Clamped", vec![decimal_exception.clone()]);
        let invalid_operation = base("InvalidOperation", vec![decimal_exception.clone()]);
        let conversion_syntax = base("ConversionSyntax", vec![invalid_operation.clone()]);
        let division_impossible = base("DivisionImpossible", vec![invalid_operation.clone()]);
        let division_undefined = base("DivisionUndefined", vec![invalid_operation.clone(), zero_div.clone()]);
        let invalid_context = base("InvalidContext", vec![invalid_operation.clone()]);
        let division_by_zero = base("DivisionByZero", vec![decimal_exception.clone(), zero_div.clone()]);
        let inexact = base("Inexact", vec![decimal_exception.clone()]);
        let rounded = base("Rounded", vec![decimal_exception.clone()]);
        let subnormal = base("Subnormal", vec![decimal_exception.clone()]);
        let overflow = base("Overflow", vec![inexact.clone(), rounded.clone()]);
        let underflow = base("Underflow", vec![inexact.clone(), rounded.clone(), subnormal.clone()]);
        let type_error = vm.ctx.exceptions.type_error.to_owned();
        let float_operation = base("FloatOperation", vec![decimal_exception.clone(), type_error]);
        DecExc {
            decimal_exception,
            clamped,
            invalid_operation,
            conversion_syntax,
            division_impossible,
            division_undefined,
            invalid_context,
            division_by_zero,
            inexact,
            overflow,
            underflow,
            rounded,
            subnormal,
            float_operation,
        }
    });
}

/// Build a `DecimalException` instance for a trap/flag.
pub(crate) fn new_decimal_error(vm: &VirtualMachine, name: &str, msg: &str) -> PyBaseExceptionRef {
    let ty: PyTypeRef = match name {
        "InvalidOperation" => exc().invalid_operation.clone(),
        "DivisionByZero" => exc().division_by_zero.clone(),
        "Overflow" => exc().overflow.clone(),
        "Underflow" => exc().underflow.clone(),
        "Inexact" => exc().inexact.clone(),
        "Rounded" => exc().rounded.clone(),
        "Clamped" => exc().clamped.clone(),
        "Subnormal" => exc().subnormal.clone(),
        "FloatOperation" => exc().float_operation.clone(),
        _ => exc().decimal_exception.clone(),
    };
    // CPython's trapped-condition exceptions carry the list of conditions as
    // their only argument: str(DecimalException) is "[<class
    // 'decimal.InvalidOperation'>]", not a human-readable sentence. The
    // condition classes themselves are what the list repr renders.
    let _ = msg;
    let conditions = vm.ctx.new_list(vec![ty.clone().into()]);
    let args = vec![conditions.into()];
    vm.new_exception(ty.clone(), args)
}

/// A decimal context snapshot used during arithmetic.
pub(crate) struct Ctx {
    pub prec: isize,
    pub rounding: Rounding,
    pub emax: isize,
    pub emin: isize,
    pub clamp: isize,
    pub flags: PyDictRef,
    pub traps: PyDictRef,
}

impl Ctx {
    pub(crate) fn from_parts(
        prec: isize,
        rounding: Rounding,
        emax: isize,
        emin: isize,
        clamp: isize,
        flags: PyDictRef,
        traps: PyDictRef,
    ) -> Self {
        Self { prec, rounding, emax, emin, clamp, flags, traps }
    }

    pub(crate) fn set_flag(&self, vm: &VirtualMachine, name: &str) -> PyResult<()> {
        let ty: &PyTypeRef = match name {
            "InvalidOperation" => &exc().invalid_operation,
            "DivisionByZero" => &exc().division_by_zero,
            "Overflow" => &exc().overflow,
            "Underflow" => &exc().underflow,
            "Inexact" => &exc().inexact,
            "Rounded" => &exc().rounded,
            "Clamped" => &exc().clamped,
            "Subnormal" => &exc().subnormal,
            "FloatOperation" => &exc().float_operation,
            _ => &exc().decimal_exception,
        };
        let key = ty.as_object();
        // Condition flags are sticky, so once one is raised there is nothing to
        // record. Checking first turns the common "already set" case into a
        // single lookup instead of an allocation plus a dict insert, which
        // otherwise dominates the cost of inexact arithmetic.
        if let Ok(value) = self.flags.get_item(key, vm) {
            if value.is_true(vm).unwrap_or(false) {
                return Ok(());
            }
        }
        self.flags.set_item(key, vm.ctx.new_bool(true).into(), vm)?;
        Ok(())
    }
    pub(crate) fn trap(&self, vm: &VirtualMachine, name: &str) -> PyResult<bool> {
        let ty: &PyTypeRef = match name {
            "InvalidOperation" => &exc().invalid_operation,
            "DivisionByZero" => &exc().division_by_zero,
            "Overflow" => &exc().overflow,
            "Underflow" => &exc().underflow,
            "Inexact" => &exc().inexact,
            "Rounded" => &exc().rounded,
            "Clamped" => &exc().clamped,
            "Subnormal" => &exc().subnormal,
            "FloatOperation" => &exc().float_operation,
            _ => &exc().decimal_exception,
        };
        // A trap only fires when the signal has been raised (flag set) AND
        // the trap for it is enabled.
        let flag_on = self
            .flags
            .get_item_opt(ty.as_object(), vm)?
            .map(|v| v.try_to_bool(vm))
            .unwrap_or(Ok(false))?;
        if !flag_on {
            return Ok(false);
        }
        self.traps
            .get_item_opt(ty.as_object(), vm)?
            .map(|v| v.try_to_bool(vm))
            // A signal absent from the traps dict is not trapped.
            .unwrap_or(Ok(false))
    }
}

/// All flags/traps that the decimal module knows about, in CPython's
/// canonical order.
const FLAG_NAMES: &[&str] = &[
    "InvalidOperation",
    "FloatOperation",
    "DivisionByZero",
    "DivisionImpossible",
    "DivisionUndefined",
    "InvalidContext",
    "Rounded",
    "Clamped",
    "Subnormal",
    "Underflow",
    "Overflow",
    "Inexact",
];

/// Round `coeff` (non-negative) down/up to `prec` significant digits.
/// Returns (rounded_coeff, exp_after, inexact). `coeff` is guaranteed to have
/// more than `prec` digits when called.
pub(crate) fn round_to_prec(
    vm: &VirtualMachine,
    ctx: &Ctx,
    sign: bool,
    coeff: &BigInt,
    exp: isize,
    prec: isize,
) -> PyResult<(BigInt, isize, bool)> {
    let digits = coeff_digits_of(coeff);
    if digits as isize <= prec {
        return Ok((coeff.clone(), exp, false));
    }
    let discard = digits as isize - prec;
    let pow10 = BigInt::from(10).pow(discard as u32);
    let (q, r) = (coeff / &pow10, coeff % &pow10);
    let inexact = r != BigInt::from(0);

    let round_up = round_up_decision(ctx.rounding, sign, &q, &r, &pow10);

    let new_coeff = if round_up { q + BigInt::from(1) } else { q };
    Ok((new_coeff, exp + discard, inexact))
}

/// Decide whether to round the quotient up, given the context rounding mode,
/// the sign of the value, the kept part ``q`` and the discarded part ``r``
/// divided by ``pow10``. Shared by round_to_prec and the underflow clamp.
fn round_up_decision(
    rounding: Rounding,
    sign: bool,
    q: &BigInt,
    r: &BigInt,
    pow10: &BigInt,
) -> bool {
    let two = BigInt::from(2);
    match rounding {
        Rounding::Down => false,
        // Directed rounding only moves the result when the discarded
        // digits are non-zero; an all-zero remainder is already exact
        // (ceiling of 1.1123450 at prec 7 is 1.112345, not 1.112346).
        Rounding::Up => *r != BigInt::from(0),
        Rounding::Ceiling => !sign && *r != BigInt::from(0),
        Rounding::Floor => sign && *r != BigInt::from(0),
        Rounding::HalfDown => (r * &two) > *pow10,
        Rounding::HalfUp => (r * &two) >= *pow10,
        Rounding::HalfEven => {
            if (r * &two) > *pow10 {
                true
            } else if (r * &two) < *pow10 {
                false
            } else {
                // tie: round to even
                (q % &two) == BigInt::from(1)
            }
        }
        Rounding::Half05Up => {
            // ROUND_05UP: round toward zero; if the last retained digit is 0
            // or 5, round away from zero instead. An exact division never
            // rounds up: add(12345, 0.000) -> 12345.
            if *r == BigInt::from(0) {
                false
            } else {
                let last = (q % BigInt::from(10)) == BigInt::from(0)
                    || (q % BigInt::from(10)) == BigInt::from(5);
                last
            }
        }
        Rounding::Half01Up => {
            // ROUND_01UP: like 05UP but the last retained digit 0 or 1
            // triggers the away-from-zero rounding.
            if *r == BigInt::from(0) {
                false
            } else {
                let last = (q % BigInt::from(10)) == BigInt::from(0)
                    || (q % BigInt::from(10)) == BigInt::from(1);
                last
            }
        }
    }
}

pub(crate) fn coeff_digits_of(coeff: &BigInt) -> usize {
    let mut s = coeff.to_string();
    if s == "0" { return 1; }
    let t = s.trim_start_matches('0');
    if t.is_empty() { 1 } else { t.len() }
}

/// Number of trailing decimal zeros in a coefficient (e.g. 1200 -> 2).
fn trailing_zeros_of(coeff: &BigInt) -> usize {
    let s = coeff.to_string();
    s.chars().rev().take_while(|&c| c == '0').count()
}

/// Apply the context to a finite result: round to precision, apply exponent
/// limits, and set Inexact/Rounded/Clamped/Underflow/Overflow flags.
pub(crate) fn apply_context(
    vm: &VirtualMachine,
    ctx: &Ctx,
    mut dec: Dec,
) -> PyResult<Dec> {
    if dec.is_special() {
        return Ok(dec);
    }
    // Zero still needs exponent-range handling: '0e-999' with a small
    // Emin gets clamped (Clamped signal) even though the value is zero.
    if dec.is_zero() {
        let etiny = ctx.emin - (ctx.prec as isize - 1);
        if dec.exp < etiny {
            ctx.set_flag(vm, "Clamped")?;
            if ctx.trap(vm, "Clamped")? {
                return Err(ctx_trap_exc(vm, "Clamped", "clamped"));
            }
            dec.exp = etiny;
        } else {
            // Clamp a too-large positive exponent of a zero: with clamp=1 the
            // cap is etop (Emax - prec + 1; clam412: plus(0E+400) -> 0E+369,
            // ddrem426: remainder(1E+384, 5E+383) -> 0E+369 at prec 16/Emax
            // 384), otherwise Emax (CPython: plus(0E+400) at clamp=0 ->
            // 0E+384, Clamped). A zero below the cap keeps its exponent
            // (plus(0E+368) at clamp=1 -> 0E+368, no flags).
            let cap = if ctx.clamp == 1 { ctx.emax - ctx.prec + 1 } else { ctx.emax };
            if dec.exp > cap {
                ctx.set_flag(vm, "Clamped")?;
                if ctx.trap(vm, "Clamped")? {
                    return Err(ctx_trap_exc(vm, "Clamped", "clamped"));
                }
                dec.exp = cap;
            }
        }
        return Ok(dec);
    }
    let (new_coeff, new_exp, inexact) =
        round_to_prec(vm, ctx, dec.sign, &dec.coeff, dec.exp, ctx.prec)?;
    if inexact || new_coeff != dec.coeff {
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
    // Remember the pre-rounding value: underflow clamps must round from the
    // exact value in one step, otherwise a tie created by the precision
    // rounding can be double-rounded (addx1012: 152445000000000000001e-100
    // at prec 5 rounds to 15245e-84, then a second clamp at Etiny rounds
    // 1524|5 up to 1525 instead of the single-round 1524|45 -> 1524).
    let orig_coeff = dec.coeff.clone();
    let orig_exp = dec.exp;
    dec.coeff = new_coeff;
    dec.exp = new_exp;
    // Carry rounding may push the coefficient to prec+1 digits (e.g.
    // 9.99999999923 at prec 9 -> 10.00000000); CPython normalizes the
    // trailing carry zeros back to prec digits (10.0000000, exponent +1).
    // The value is unchanged; only the representation is adjusted.
    while coeff_digits_of(&dec.coeff) > ctx.prec as usize
        && dec.coeff != BigInt::from(0)
        && (&dec.coeff % BigInt::from(10)) == BigInt::from(0)
    {
        dec.coeff = &dec.coeff / BigInt::from(10);
        dec.exp += 1;
    }
    // clamp=1: when the exponent exceeds etop (Emax - prec + 1), the
    // coefficient is padded with zeros down to etop (Clamped signal).
    // e.g. plus(1E+99) with clamp=1, Emax=99, prec=28 ->
    // 1.000000000000000000000000000E+99.
    // Clamp applies only to values that still fit within Emax: an overflow
    // (adjusted exponent > Emax) is decided below instead of signaling
    // Clamped (CPython: ddadd81621 add -9999999999999999E+369 -9E+369 ->
    // -9.999999999999999E+384 Overflow, no Clamped).
    if ctx.clamp == 1 && new_exp > ctx.emax - ctx.prec + 1 {
        let adj_after = (dec.exp as i128) + coeff_digits_of(&dec.coeff) as i128 - 1;
        if adj_after <= ctx.emax as i128 {
            let etop = ctx.emax - ctx.prec + 1;
            let shift = new_exp - etop;
            let pow = BigInt::from(10).pow(shift as u32);
            dec.coeff = &dec.coeff * &pow;
            dec.exp = etop;
            ctx.set_flag(vm, "Clamped")?;
            if ctx.trap(vm, "Clamped")? {
                return Err(ctx_trap_exc(vm, "Clamped", "clamped"));
            }
        }
    }
    // exponent range check
    let etiny = ctx.emin - (ctx.prec as isize - 1);
    // Use dec.exp (post carry-normalization) for the adjusted exponent; the
    // carry loop above can move trailing zeros into the exponent (addx578:
    // 99999999999999999e-400 rounds to 1000000000000000e-398 whose adjusted
    // exponent is -383 == Emin, not subnormal).
    let adj_exp = (dec.exp as i128) + coeff_digits_of(&dec.coeff) as i128 - 1;
    if adj_exp < etiny as i128 {
        // Underflow / Subnormal: mpdecimal decides Underflow on the *adjusted*
        // exponent (exp + digits - 1) falling below Etiny, not the stored
        // exponent. 100E-1002 (prec=3, Emin=-999) has adjusted -1000,
        // Etiny -1001 -> no underflow, only Subnormal.
        ctx.set_flag(vm, "Subnormal")?;
        ctx.set_flag(vm, "Underflow")?;
        if ctx.trap(vm, "Underflow")? {
            return Err(ctx_trap_exc(vm, "Underflow", "underflow"));
        }
        // clamp to etiny (round per context mode, mpdecimal-style: 0.009E-999
        // at Etiny rounds to 1E-1001 under half_up; -0.011 under floor sits
        // at -1E-398, not flushed to zero). Round from the exact pre-rounding
        // value in a single step to avoid double rounding.
        let shift = etiny - orig_exp;
        let digits = coeff_digits_of(&orig_coeff) as isize;
        // Fast path: when shift exceeds the operand's digit count the quotient
        // is zero (the whole coefficient is the remainder), so 10^shift is
        // never needed as a divisor. This matters for ultra-tiny operands such
        // as 1E-999999999, where 10^shift would otherwise materialise a number
        // with ~10^9 digits. Here r < 10^digits and pow = 10^shift with
        // shift > digits, so r*2 < pow holds for every half-* mode.
        if shift > digits {
            let r = orig_coeff.clone();
            let round_up = match ctx.rounding {
                Rounding::Down => false,
                Rounding::Up => true,
                Rounding::Ceiling => !dec.sign,
                Rounding::Floor => dec.sign,
                Rounding::HalfDown | Rounding::HalfUp | Rounding::HalfEven => false,
                // q == 0, last kept digit is 0: 05up/01up round up regardless.
                Rounding::Half05Up | Rounding::Half01Up => true,
            };
            dec.coeff = if round_up { BigInt::from(1) } else { BigInt::from(0) };
            dec.exp = etiny;
            if r != BigInt::from(0) {
                ctx.set_flag(vm, "Inexact")?;
                ctx.set_flag(vm, "Rounded")?;
            }
            // Clamped only when the clamped result is zero (the value was
            // flushed to zero at Etiny).
            if dec.coeff == BigInt::from(0) {
                ctx.set_flag(vm, "Clamped")?;
                if ctx.trap(vm, "Clamped")? {
                    return Err(ctx_trap_exc(vm, "Clamped", "clamped"));
                }
            }
        } else {
            let pow = BigInt::from(10).pow(shift as u32);
            let (q, r) = (&orig_coeff / &pow, &orig_coeff % &pow);
            let round_up = round_up_decision(ctx.rounding, dec.sign, &q, &r, &pow);
            dec.coeff = if round_up { q + BigInt::from(1) } else { q };
            dec.exp = etiny;
            if r != BigInt::from(0) {
                ctx.set_flag(vm, "Inexact")?;
                ctx.set_flag(vm, "Rounded")?;
            }
            // Clamped only when the clamped result is zero (the value was
            // flushed to zero at Etiny): 0.001E-999 -> 0E-1001 is Clamped,
            // while 0.009E-999 -> 1E-1001 (rounds up to a nonzero subnormal)
            // is not.
            if dec.coeff == BigInt::from(0) {
                ctx.set_flag(vm, "Clamped")?;
                if ctx.trap(vm, "Clamped")? {
                    return Err(ctx_trap_exc(vm, "Clamped", "clamped"));
                }
            }
        }
    } else if (orig_exp as i128) + coeff_digits_of(&orig_coeff) as i128 - 1 < ctx.emin as i128 {
        // Subnormal: the *adjusted* exponent (exp + digits - 1) is below Emin
        // but at/above Etiny, so no underflow; the Subnormal signal is set. A
        // zero coefficient is not subnormal (CPython: only nonzero values get
        // the Subnormal flag). Using the adjusted exponent matters at the
        // boundary: 1.00E-999 with prec=3/Emin=-999 has adjusted exponent -999
        // == Emin (not subnormal), while 1E-1000 has adjusted -1000 < Emin
        // (subnormal). Judged on the pre-rounding value: carry normalization
        // may lift dec.exp to Etiny (addx6578: 99999999999999999e-400 rounds
        // to 1000000000000000e-398) so the subnormal decision must use orig.
        if !dec.is_zero() {
            ctx.set_flag(vm, "Subnormal")?;
            if ctx.trap(vm, "Subnormal")? {
                return Err(ctx_trap_exc(vm, "Subnormal", "subnormal"));
            }
            // mpdecimal normalises subnormal results by stripping trailing
            // zeros until the *stored* exponent reaches Etiny (value
            // unchanged, so Rounded but not Inexact). 100E-1002 with
            // prec=3/Emin=-999/Etiny=-1001 strips one zero -> 10E-1001
            // (1.0E-1000), flagged Rounded+Subnormal. If stripping all
            // trailing zeros still leaves the exponent below Etiny, the value
            // cannot be represented exactly and it falls through to the
            // underflow/clamp handling below.
            let k = trailing_zeros_of(&orig_coeff);
            if (orig_exp + k as isize) < etiny {
                // Underflow: even at minimum precision the exponent is below
                // Etiny; round to Etiny (inexact -> Inexact+Rounded). No
                // Clamped here: the result is an ordinary rounding up into the
                // subnormal Etiny range, not a zero clamp (CPython 0.999E-999
                // -> 1.00E-999 with Inexact Rounded Subnormal Underflow).
                // Judged on the pre-rounding value: carry normalization can
                // lift dec.exp to Etiny (addx6580: 9999999999999999999e-402
                // rounds to 1000000000000000e-398), hiding the underflow.
                ctx.set_flag(vm, "Subnormal")?; // already set above
                ctx.set_flag(vm, "Underflow")?;
                if ctx.trap(vm, "Underflow")? {
                    return Err(ctx_trap_exc(vm, "Underflow", "underflow"));
                }
                let shift = (etiny - orig_exp) as usize;
                let pow = BigInt::from(10).pow(shift as u32);
                let (q, r) = (&orig_coeff / &pow, &orig_coeff % &pow);
                // Round per the context mode (mpdecimal rounds a subnormal to
                // the closest representable at Etiny); single-step from the
                // exact value to avoid double rounding (addx1012).
                let round_up = round_up_decision(ctx.rounding, dec.sign, &q, &r, &pow);
                dec.coeff = if round_up { q + BigInt::from(1) } else { q };
                dec.exp = etiny;
                ctx.set_flag(vm, "Inexact")?;
                ctx.set_flag(vm, "Rounded")?;
            } else if dec.exp < etiny {
                // Strip trailing zeros up to Etiny (value-preserving), Rounded.
                let strip = (etiny - dec.exp) as usize;
                let strip = strip.min(k);
                if strip > 0 {
                    let pow = BigInt::from(10).pow(strip as u32);
                    dec.coeff = &dec.coeff / &pow;
                    dec.exp = etiny;
                    ctx.set_flag(vm, "Rounded")?;
                    if ctx.trap(vm, "Rounded")? {
                        return Err(ctx_trap_exc(vm, "Rounded", "rounded result"));
                    }
                }
            }
        }
    } else if (dec.exp as i128) + coeff_digits_of(&dec.coeff) as i128 - 1 > ctx.emax as i128 {
        // Overflow is determined by the adjusted exponent, not just the
        // stored coefficient exponent: 999 * 1E+425000000 has adjusted
        // exponent 425000002 even though its stored exponent is Emax.
        ctx.set_flag(vm, "Overflow")?;
        ctx.set_flag(vm, "Inexact")?;
        ctx.set_flag(vm, "Rounded")?;
        if ctx.trap(vm, "Overflow")? {
            return Err(ctx_trap_exc(vm, "Overflow", "overflow"));
        }
        // Non-trapping overflow: the clamped value depends on rounding AND
        // sign, matching CPython's Overflow.handle. half_up/even/down and
        // up -> ±Infinity; ceiling overflows to +Infinity only for a positive
        // result (negative clamps to the largest finite); floor overflows to
        // -Infinity only for a negative result (positive clamps to finite);
        // down/05up/01up always clamp to the largest finite (prec nines at
        // Emax, sign preserved).
        let to_inf = match ctx.rounding {
            Rounding::HalfUp | Rounding::HalfEven | Rounding::HalfDown | Rounding::Up => true,
            Rounding::Ceiling => !dec.sign,
            Rounding::Floor => dec.sign,
            Rounding::Down | Rounding::Half05Up | Rounding::Half01Up => false,
        };
        if to_inf {
            dec.special = Special::Infinity;
            dec.coeff = BigInt::from(0);
            dec.exp = 0;
        } else {
            let nines = BigInt::from(10).pow(ctx.prec as u32) - BigInt::from(1);
            dec.special = Special::Finite;
            dec.coeff = nines;
            dec.exp = ctx.emax - ctx.prec as isize + 1;
        }
    }
    Ok(dec)
}

/// Exact decimal square root (CPython _pydecimal algorithm):
/// express self = c * 100**e with 100**(l-1) <= c < 100**l, rescale c to
/// prec base-100 digits, Newton's method for floor(sqrt(c)), fix the last
/// digit, then round half-even into the context.
/// Integer square root of a non-negative BigInt (Newton's method).
fn isqrt(n: &BigInt) -> BigInt {
    if n == &BigInt::from(0) {
        return BigInt::from(0);
    }
    // Initial guess 2^ceil(bits/2) is always >= sqrt(n).
    let mut x = BigInt::from(1) << ((n.bits() + 1) / 2);
    loop {
        let y = (&x + n / &x) >> 1u32;
        if y >= x {
            break;
        }
        x = y;
    }
    x
}

pub(crate) fn dec_sqrt(vm: &VirtualMachine, ctx: &Ctx, d: &Dec) -> PyResult<Dec> {
    // sqrt(-Infinity) is InvalidOperation; +Infinity and NaN pass through.
    if d.special == Special::Infinity && d.sign {
        ctx.set_flag(vm, "InvalidOperation")?;
        if ctx.trap(vm, "InvalidOperation")? {
            return Err(new_decimal_error(vm, "InvalidOperation", "sqrt(-x), x > 0"));
        }
        return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
    }
    if d.special == Special::SignalingNaN {
        ctx.set_flag(vm, "InvalidOperation")?;
        if ctx.trap(vm, "InvalidOperation")? {
            return Err(new_decimal_error(vm, "InvalidOperation", "sNaN operand"));
        }
        // decTest sqtx824/840: the sNaN is quieted with its payload
        // preserved (sqrt(sNaN123) -> NaN123), sign kept, and the payload
        // truncated to context precision keeping the LOW digits
        // (sqrt(-sNaN1234567890) -> -NaN67890 at prec 5), matching
        // special_binary's payload handling.
        let mut n = d.clone();
        n.special = Special::NaN;
        let digits = n.coeff_str();
        let keep = (ctx.prec as usize).max(1);
        if digits.len() > keep {
            n.coeff = digits[digits.len() - keep..]
                .parse::<BigInt>()
                .unwrap_or_else(|_| BigInt::from(0));
        }
        return Ok(n);
    }
    if d.is_special() {
        return Ok(d.clone());
    }
    if d.is_zero() {
        // sqrt(-0) = -0; ideal exponent exp // 2. Floor division, not
        // truncating: sqrt(0.0) keeps one fractional place (exp -1), while
        // truncating turned -1 into 0 and printed "0" instead of "0.0".
        let mut r = d.clone();
        r.exp = d.exp.div_euclid(2);
        return apply_context(vm, ctx, r);
    }
    if d.sign {
        ctx.set_flag(vm, "InvalidOperation")?;
        if ctx.trap(vm, "InvalidOperation")? {
            return Err(new_decimal_error(vm, "InvalidOperation", "sqrt(-x), x > 0"));
        }
        return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
    }

    // Compute the root as an integer at a scale carrying `prec + 2`
    // significant digits: sqrt(value) * 10^t == sqrt(coeff * 10^(exp + 2t)).
    let prec = ctx.prec;
    let target = prec + 2;
    let digits = coeff_digits_of(&d.coeff) as isize;
    let mut t: isize = 0;
    while d.exp + 2 * t < 0 {
        t += 1;
    }
    let total = digits + d.exp + 2 * t;
    if total < 2 * target {
        t += (2 * target - total + 1) / 2;
    }
    let shift = d.exp + 2 * t;
    let n = &d.coeff * BigInt::from(10).pow(shift as u32);
    let root = isqrt(&n);
    let exact = &root * &root == n;

    let mut ans = Dec { sign: false, coeff: root, exp: -t, special: Special::Finite };
    if exact {
        // An exact root keeps the operand's ideal exponent (d.exp // 2), so
        // sqrt(1.00) is 1.0 and sqrt(1.0000) is 1.00 rather than a full-width
        // expansion; CPython keeps those trailing zeros in the coefficient.
        // Python's // floors, so -3 // 2 = -2 (sqtx078 sqrt(1.000) -> 1.00);
        // Rust's / truncates toward zero and would give -1.
        let ideal = d.exp.div_euclid(2);
        let ten = BigInt::from(10);
        while ans.exp < ideal && (&ans.coeff % &ten) == BigInt::from(0) {
            ans.coeff /= &ten;
            ans.exp += 1;
        }
    } else {
        ctx.set_flag(vm, "Inexact")?;
        if ctx.trap(vm, "Inexact")? {
            return Err(new_decimal_error(vm, "Inexact", "inexact result"));
        }
        // Sticky fold: root = isqrt(n) is the truncated root, and the true
        // value sits strictly above it by the remainder n - root**2, which
        // apply_context cannot see when rounding the prec+2-digit
        // coefficient down to prec. Folding that nonzero fraction into the
        // coefficient's last digit (only when that digit is zero, so no
        // carry) marks the digits-beyond as nonzero for the rounding:
        // sqtx3775 sqrt(0.431) = 0.6565032... must round to 0.657, not to
        // the false half-even tie 0.6565 -> 0.656.
        if ans.coeff.to_string().ends_with('0') {
            ans.coeff = &ans.coeff + BigInt::from(1);
        }
    }

    // sqrt always rounds half-even, whatever the context rounding is.
    let rctx = Ctx::from_parts(
        prec,
        Rounding::HalfEven,
        ctx.emax,
        ctx.emin,
        ctx.clamp,
        ctx.flags.clone(),
        ctx.traps.clone(),
    );
    apply_context(vm, &rctx, ans)
}
pub(crate) fn ctx_trap_exc(vm: &VirtualMachine, name: &str, _msg: &str) -> PyBaseExceptionRef {
    new_decimal_error(vm, name, &format!("[{name}]"))
}

/// A private context used only for intermediate series evaluation: wide
/// exponent range, extra precision, and throwaway flag/trap tables so the
/// caller's context never sees the rounding of internal steps.
fn working_ctx(vm: &VirtualMachine, ctx: &Ctx, prec: isize) -> Ctx {
    Ctx::from_parts(
        prec,
        ctx.rounding,
        999_999_999,
        -999_999_999,
        ctx.clamp,
        vm.ctx.new_dict(),
        vm.ctx.new_dict(),
    )
}

fn dec_from_i64(v: i64) -> Dec {
    Dec {
        sign: v < 0,
        coeff: BigInt::from(v.unsigned_abs()),
        exp: 0,
        special: Special::Finite,
    }
}

/// Fixed-point helpers for the ln/exp series. Values are carried as BigInts
/// scaled by 10^`scale`, so the inner loops do plain integer arithmetic instead
/// of going through `dec_mul`/`dec_div`, which would apply the context and
/// touch the flag dictionary on every single step.
fn scaled_of(d: &Dec, scale: usize) -> BigInt {
    let e = d.exp + scale as isize;
    let mut coeff = d.coeff.clone();
    if e >= 0 {
        coeff *= BigInt::from(10).pow(e as u32);
    } else {
        coeff /= BigInt::from(10).pow((-e) as u32);
    }
    if d.sign {
        -coeff
    } else {
        coeff
    }
}

fn scaled_mul(a: &BigInt, b: &BigInt, scale: usize) -> BigInt {
    (a * b) / BigInt::from(10).pow(scale as u32)
}

fn scaled_div(a: &BigInt, b: &BigInt, scale: usize) -> BigInt {
    (a * BigInt::from(10).pow(scale as u32)) / b
}

fn scaled_pow10(scale: usize) -> BigInt {
    BigInt::from(10).pow(scale as u32)
}

fn dec_from_scaled(value: &BigInt, scale: usize) -> Dec {
    Dec {
        sign: *value < BigInt::from(0),
        coeff: if *value < BigInt::from(0) { -value } else { value.clone() },
        exp: -(scale as isize),
        special: Special::Finite,
    }
}

/// ln of `m`, which the caller guarantees to be a finite positive value already
/// reduced to [1, 10).  Uses the atanh series
/// ``ln(m) = 2 * (t + t^3/3 + t^5/5 + ...)`` with ``t = (m - 1) / (m + 1)``,
/// which converges quickly for m near 1.
fn ln_mantissa(vm: &VirtualMachine, ctx: &Ctx, m: &Dec) -> PyResult<Dec> {
    let _ = vm;
    let scale = (ctx.prec as usize) + 12;
    let one = scaled_pow10(scale);
    let ms = scaled_of(m, scale);

    // t = (m - 1) / (m + 1)
    let num = &ms - &one;
    let den = &ms + &one;
    let t = scaled_div(&num, &den, scale);
    let t2 = scaled_mul(&t, &t, scale);

    // The caller reduces the argument into [1/sqrt(10), sqrt(10)), so |t| < 0.52
    // and t2 < 0.274: each term buys about 0.56 digits. A term that rounds to
    // zero at this scale cannot change the result, which is the exact stop.
    let max_terms: i64 = 2 * ctx.prec as i64 + 40;
    let mut term = t.clone();
    let mut sum = t;
    let mut divisor: i64 = 3;
    let mut steps: i64 = 0;
    while steps < max_terms {
        term = scaled_mul(&term, &t2, scale);
        let piece = &term / BigInt::from(divisor);
        if piece == BigInt::from(0) {
            break;
        }
        sum += &piece;
        divisor += 2;
        steps += 1;
    }
    Ok(dec_from_scaled(&(sum * BigInt::from(2)), scale))
}

/// Memo for ln(2) and ln(10). Both are functions of (precision, rounding)
/// only, and each costs a few hundred series terms, so recomputing them on
/// every call dominates the cost of ln/exp.
thread_local! {
    static LN2_CACHE: std::cell::RefCell<Vec<(isize, u8, Dec)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static LN10_CACHE: std::cell::RefCell<Vec<(isize, u8, Dec)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn cache_key(ctx: &Ctx) -> (isize, u8) {
    (ctx.prec, ctx.rounding as u8)
}

/// ln(2): t = 1/3, so the series converges geometrically.
fn ln_two(vm: &VirtualMachine, ctx: &Ctx) -> PyResult<Dec> {
    let key = cache_key(ctx);
    if let Some(hit) = LN2_CACHE.with(|c| {
        c.borrow().iter().find(|(p, r, _)| (*p, *r) == key).map(|(_, _, d)| d.clone())
    }) {
        return Ok(hit);
    }
    let value = ln_mantissa(vm, ctx, &dec_from_i64(2))?;
    LN2_CACHE.with(|c| c.borrow_mut().push((key.0, key.1, value.clone())));
    Ok(value)
}

/// ln(10) = 3*ln(2) + ln(1.25); both halves have small |t|.
fn ln_ten(vm: &VirtualMachine, ctx: &Ctx) -> PyResult<Dec> {
    let key = cache_key(ctx);
    if let Some(hit) = LN10_CACHE.with(|c| {
        c.borrow().iter().find(|(p, r, _)| (*p, *r) == key).map(|(_, _, d)| d.clone())
    }) {
        return Ok(hit);
    }
    let value = ln_ten_uncached(vm, ctx)?;
    LN10_CACHE.with(|c| c.borrow_mut().push((key.0, key.1, value.clone())));
    Ok(value)
}

fn ln_ten_uncached(vm: &VirtualMachine, ctx: &Ctx) -> PyResult<Dec> {
    let three_ln2 = dec_mul(ctx, vm, &ln_two(vm, ctx)?, &dec_from_i64(3))?;
    // 1.25 = 5/4
    let five = dec_from_i64(5);
    let four = dec_from_i64(4);
    let ln_125 = ln_mantissa(vm, ctx, &dec_div(ctx, vm, &five, &four)?)?;
    dec_add(ctx, vm, &three_ln2, &ln_125)
}

/// Natural logarithm of a positive finite decimal, correctly rounded to the
/// caller's context.
/// The unary-op sNaN handling shared by ln/log10/exp (decTest lnx823 and
/// friends): signal InvalidOperation, then quiet the operand with its
/// payload preserved (truncated to the context precision), matching sqrt
/// (sqtx824/840). Returns None when the operand is not a signaling NaN.
pub(crate) fn quiet_snan_unary(vm: &VirtualMachine, ctx: &Ctx, d: &Dec) -> PyResult<Option<Dec>> {
    if d.special != Special::SignalingNaN {
        return Ok(None);
    }
    ctx.set_flag(vm, "InvalidOperation")?;
    if ctx.trap(vm, "InvalidOperation")? {
        return Err(new_decimal_error(vm, "InvalidOperation", "sNaN operand"));
    }
    let mut n = d.clone();
    n.special = Special::NaN;
    let digits = n.coeff_str();
    let keep = (ctx.prec as usize).max(1);
    if digits.len() > keep {
        n.coeff = digits[digits.len() - keep..]
            .parse::<BigInt>()
            .unwrap_or_else(|_| BigInt::from(0));
    }
    Ok(Some(n))
}

pub(crate) fn dec_ln(vm: &VirtualMachine, ctx: &Ctx, d: &Dec) -> PyResult<Dec> {
    if let Some(n) = quiet_snan_unary(vm, ctx, d)? {
        return Ok(n);
    }
    // A quiet NaN passes through with its sign and payload BEFORE the
    // negative check: decTest lnx827 ln(-NaN654) -> -NaN654 with no signal
    // (the NaN is not a negative number).
    if d.special == Special::NaN {
        return Ok(d.clone());
    }
    // ln(0) is -Infinity, not 0. The Context methods handled this themselves,
    // but callers inside the core (notably the fractional power path, which
    // computes exp(b * ln(a))) went straight to dec_ln and saw 0 instead, so
    // 0 ** 0.5 came back as exp(0) == 1.
    if d.is_zero() {
        return Ok(Dec {
            sign: false,
            coeff: BigInt::from(0),
            exp: 0,
            special: Special::Infinity,
        });
    }
    if d.sign {
        // ln(-x) is InvalidOperation, not the operand.
        ctx.set_flag(vm, "InvalidOperation")?;
        if ctx.trap(vm, "InvalidOperation")? {
            return Err(new_decimal_error(vm, "InvalidOperation", "ln of a negative number"));
        }
        return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
    }
    if d.is_special() {
        return Ok(d.clone());
    }
    let wctx = working_ctx(vm, ctx, ctx.prec + 12);

    // Split into m * 10^k with m in [1, 10): the leading digit sequence is the
    // mantissa and the remaining scale becomes the power of ten.
    let digits = d.coeff_digits() as isize;
    let mut k = d.exp + digits - 1;
    let mut m = Dec {
        sign: false,
        coeff: d.coeff.clone(),
        exp: -(digits - 1),
        special: Special::Finite,
    };

    // One more step of reduction into [1/sqrt(10), sqrt(10)). It costs a single
    // division but roughly halves |t|, and the atanh series then needs far
    // fewer terms. Comparing against sqrt(10) is done on the digits so that no
    // comparison helper is needed: m >= 3.2 <=> coeff >= 32 * 10^(digits-2),
    // or coeff >= 4 for a single-digit mantissa.
    let over_sqrt10 = if digits >= 2 {
        m.coeff >= BigInt::from(32) * BigInt::from(10).pow((digits - 2) as u32)
    } else {
        m.coeff >= BigInt::from(4)
    };
    if over_sqrt10 {
        m = dec_div(&wctx, vm, &m, &dec_from_i64(10))?;
        k += 1;
    }

    let lnm = ln_mantissa(vm, &wctx, &m)?;
    let result = if k == 0 {
        lnm
    } else {
        let scaled = dec_mul(&wctx, vm, &ln_ten(vm, &wctx)?, &dec_from_i64(k as i64))?;
        dec_add(&wctx, vm, &lnm, &scaled)?
    };
    // ln(1) is exactly 0, not a zero carrying the series scale.
    if result.is_zero() {
        return Ok(Dec::zero());
    }
    apply_context(vm, ctx, result)
}

/// Base-10 logarithm, as ln(x) / ln(10).
pub(crate) fn dec_log10(vm: &VirtualMachine, ctx: &Ctx, d: &Dec) -> PyResult<Dec> {
    if let Some(n) = quiet_snan_unary(vm, ctx, d)? {
        return Ok(n);
    }
    // log10(0) is -Infinity, for the same reason ln(0) is: see dec_ln.
    if d.is_zero() {
        return Ok(Dec {
            sign: false,
            coeff: BigInt::from(0),
            exp: 0,
            special: Special::Infinity,
        });
    }
    if d.sign {
        // log10 of a negative number is InvalidOperation - but a quiet NaN
        // passes through with its sign and payload BEFORE this check
        // (decTest logx...: ln -NaN654 -> -NaN654, no signal).
        if d.special == Special::NaN {
            return Ok(d.clone());
        }
        ctx.set_flag(vm, "InvalidOperation")?;
        if ctx.trap(vm, "InvalidOperation")? {
            return Err(new_decimal_error(vm, "InvalidOperation", "ln of a negative number"));
        }
        return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
    }
    if d.is_special() {
        return Ok(d.clone());
    }
    // decTest logx1103/logxs001: log10 of an exact power of ten is the exact
    // integer with NO flags. The series quotient of two prec+12-digit
    // approximations is never exactly 3.000..., so detect the power of ten
    // in the OPERAND: coeff == 10**m means log10(coeff * 10**exp) = m + exp.
    // Still round through apply_context: at precision 1 the exact 10 becomes
    // 1E+1 with Rounded (logx1141), and log10(1E+13) = 13 gives Inexact too
    // (logx1142).
    {
        let s = d.coeff.to_string();
        if s.starts_with('1') && s[1..].chars().all(|c| c == '0') {
            let m = (s.len() - 1) as isize;
            let k = m + d.exp;
            return apply_context(
                vm,
                ctx,
                Dec {
                    sign: k < 0,
                    coeff: BigInt::from(k.unsigned_abs()),
                    exp: 0,
                    special: Special::Finite,
                },
            );
        }
    }
    let wctx = working_ctx(vm, ctx, ctx.prec + 12);
    let ln_x = dec_ln(vm, &wctx, d)?;
    let ln_10 = ln_ten(vm, &wctx)?;
    let r = dec_div(&wctx, vm, &ln_x, &ln_10)?;
    if r.is_zero() {
        return Ok(Dec::zero());
    }
    // The quotient for an exact power of ten is the exact integer (ln(1000)
    // decomposes to 3*ln(10), so the division is exact). Anything else is
    // inexact somewhere below the working precision, and an inexact result
    // that merely ROUNDS to an integer keeps its rounded shape:
    // logx1331 log10(999.9999998) at prec 9 is 3.00000000 Inexact Rounded,
    // not the trimmed 3. Only an exact integer trims to exponent 0.
    let exact_int = r.exp >= 0 || {
        let scale = BigInt::from(10).pow((-r.exp) as u32);
        (&r.coeff % &scale) == BigInt::from(0)
    };
    let r = apply_context(vm, ctx, r)?;
    if !r.is_zero() && exact_int {
        // CPython expresses an exact integral result with exponent 0:
        // log10(100) is 2, log10(1E+10) is 10. Drop the series scale's
        // trailing zeros, then pull a positive exponent back into the
        // coefficient.
        let ten = BigInt::from(10);
        let mut coeff = r.coeff;
        let mut exp = r.exp;
        while (&coeff % &ten) == BigInt::from(0) {
            coeff /= &ten;
            exp += 1;
        }
        if exp > 0 {
            coeff *= BigInt::from(10).pow(exp as u32);
            exp = 0;
        }
        return Ok(Dec { sign: r.sign, coeff, exp, special: Special::Finite });
    }
    if !r.is_zero() && r.exp < 0 {
        // Only a FRACTIONAL inexact result is padded to the context width:
        // log10(7) at prec 5 is 0.84510. An integral one is not.
        let digits = coeff_digits_of(&r.coeff);
        if digits < ctx.prec as usize {
            let pad = ctx.prec as usize - digits;
            return Ok(Dec {
                sign: r.sign,
                coeff: &r.coeff * &BigInt::from(10).pow(pad as u32),
                exp: r.exp - pad as isize,
                special: Special::Finite,
            });
        }
    }
    Ok(r)
}
/// e**x for a finite decimal, correctly rounded to the caller's context.
pub(crate) fn dec_exp(vm: &VirtualMachine, ctx: &Ctx, d: &Dec) -> PyResult<Dec> {
    if let Some(n) = quiet_snan_unary(vm, ctx, d)? {
        return Ok(n);
    }
    // NaN propagates with its payload (decTest expx826: exp NaN456 ->
    // NaN456), -Infinity underflows to zero, +Infinity overflows.
    if d.special == Special::NaN {
        return Ok(d.clone());
    }
    if d.special == Special::Infinity {
        if d.sign {
            return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Finite });
        }
        // exp(+Infinity) is Infinity and signals nothing - it is an input,
        // not an overflow produced by this operation.
        return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
    }
    // exp(0) is exactly 1 and keeps exponent 0; the series would
    // otherwise hand back a full-width 1.0000.
    if d.is_zero() {
        return Ok(Dec { sign: false, coeff: BigInt::from(1), exp: 0, special: Special::Finite });
    }
    // NB: do not test magnitude via `d.exp` here - that is the decimal
    // exponent, so exp(0.231...) has exp -25 and looks "tiny" while being an
    // ordinary value. Range handling belongs in apply_context.
    let wctx = working_ctx(vm, ctx, ctx.prec + 12);
    let ln2 = ln_two(vm, &wctx)?;

    // x = n*ln2 + r, so that |r| <= ln2/2 and the Taylor series converges fast.
    let n = round_to_int(&dec_div(&wctx, vm, d, &ln2)?);
    // exp(x) = 2**n * exp(r); the result's magnitude is set by n. `n.coeff`
    // is a magnitude, so apply `n.sign`; and compare the *decimal* exponent
    // of the result (about n*log10(2)) against the context exponent limits.
    // Comparing the base-2 exponent against emax is dimensionally wrong and
    // over/under-flows at the wrong magnitudes. The exact clamped-zero /
    // subnormal / rounding is left to apply_context; this guard only
    // short-circuits the clearly out-of-range cases so pow2 does not build a
    // multi-megabyte exponentiation.
    let n_int = match n.coeff.to_string().parse::<isize>() {
        Ok(mag) => {
            if n.sign { mag.saturating_neg() } else { mag }
        }
        // n does not fit in an isize (e.g. exp(1e300)): it is astronomically
        // large in magnitude. Saturate by sign so the range test below decides
        // overflow vs underflow instead of falling through (which used to
        // attempt an impossible exponentiation and yield -Infinity).
        Err(_) => {
            if n.sign { isize::MIN } else { isize::MAX }
        }
    };
    const LOG10_2: f64 = 0.301_029_995_663_981_2;
    let approx_dec_exp = n_int as f64 * LOG10_2;
    let etiny = (ctx.emin - (ctx.prec as isize - 1)) as f64;
    if approx_dec_exp > ctx.emax as f64 + 10.0 {
        ctx.set_flag(vm, "Overflow")?;
        if ctx.trap(vm, "Overflow")? {
            return Err(new_decimal_error(vm, "Overflow", "exp overflow"));
        }
        // decTest expx047: exp 1000 -> Infinity Overflow Inexact Rounded. The
        // overflow discards the whole coefficient, so Inexact and Rounded
        // accompany Overflow (the add path's overflow produces all three).
        ctx.set_flag(vm, "Inexact")?;
        if ctx.trap(vm, "Inexact")? {
            return Err(new_decimal_error(vm, "Inexact", "inexact result"));
        }
        ctx.set_flag(vm, "Rounded")?;
        if ctx.trap(vm, "Rounded")? {
            return Err(new_decimal_error(vm, "Rounded", "rounded result"));
        }
        return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
    }
    if approx_dec_exp < etiny - 10.0 {
        // definitely below Etiny. Feed apply_context a representative tiny
        // nonzero value (not a literal 0) so it produces CPython's clamped
        // underflowed zero 0E+Etiny together with the Underflow/Inexact
        // signals; a literal 0 would come back as 0E+0.
        return apply_context(
            vm,
            ctx,
            Dec {
                sign: false,
                coeff: BigInt::from(1),
                exp: ctx.emin - ctx.prec as isize - 5,
                special: Special::Finite,
            },
        );
    }
    let scale = (wctx.prec as usize) + 12;
    let xs = scaled_of(d, scale);
    let ln2s = scaled_of(&ln2, scale);
    let ns = scaled_of(&n, scale);
    let r = &xs - &scaled_mul(&ln2s, &ns, scale);

    let one = scaled_pow10(scale);
    let mut term = one.clone();
    let mut sum = one;
    let mut k: i64 = 1;
    // |r| <= ln2/2, so each term of r^k/k! shrinks by well under a factor
    // of 0.35; a few dozen terms already exceed any realistic precision, and
    // a term that rounds to zero at this scale cannot change the result.
    let max_terms: i64 = wctx.prec as i64 + 40;
    while k < max_terms {
        term = scaled_mul(&term, &r, scale);
        term /= BigInt::from(k);
        if term == BigInt::from(0) {
            break;
        }
        sum += &term;
        k += 1;
    }
let p2 = pow2(&wctx, vm, dec_int_saturating(&n))?;
// Multiply by 2**n through the Dec representation rather than the fixed
// scaled-integer form. For a large-magnitude negative argument 2**n has a
// very negative decimal exponent (2**-144 ~ 1e-44, exponent -83), and
// `scaled_of` would truncate it against `scale`, costing significant
// digits (exp(-100) dropped to 9 significant digits instead of 28).
let exp_r = dec_from_scaled(&sum, scale);
let result = dec_mul(&wctx, vm, &exp_r, &p2)?;
let result = apply_context(vm, ctx, result)?;
// exp(x) for a finite nonzero x is transcendental, so the result is
// never exactly representable: every decTest finite case carries Inexact
// (expx211: exp -1E-40 at prec 50 -> ... Inexact Rounded, even though the
// correction 5E-81 sits below the working precision). Signal after the
// rounding so a trapped Inexact raises instead of returning.
ctx.set_flag(vm, "Inexact")?;
if ctx.trap(vm, "Inexact")? {
    return Err(new_decimal_error(vm, "Inexact", "inexact result"));
}
ctx.set_flag(vm, "Rounded")?;
if ctx.trap(vm, "Rounded")? {
    return Err(new_decimal_error(vm, "Rounded", "rounded result"));
}
Ok(result)
}

/// Integer value of an integer-valued decimal, saturating at the i64 range.
fn dec_int_saturating(d: &Dec) -> i64 {
    if d.is_special() || d.is_zero() {
        return 0;
    }
    let mut coeff = d.coeff.clone();
    let mut e = d.exp;
    while e > 0 {
        if coeff > BigInt::from(u32::MAX) {
            return if d.sign { i64::MIN } else { i64::MAX };
        }
        coeff *= 10u32;
        e -= 1;
    }
    let mut value: i64 = 0;
    for ch in coeff.to_string().chars() {
        let digit = ch as u8 - b'0';
        match value
            .checked_mul(10)
            .and_then(|x| x.checked_add(i64::from(digit)))
        {
            Some(x) => value = x,
            None => return if d.sign { i64::MIN } else { i64::MAX },
        }
    }
    if d.sign { -value } else { value }
}

/// 2**n, by exponentiation by squaring.
fn pow2(ctx: &Ctx, vm: &VirtualMachine, n: i64) -> PyResult<Dec> {
    let magnitude = n.unsigned_abs();
    let mut acc = dec_from_i64(1);
    let mut base = dec_from_i64(2);
    let mut e = magnitude;
    while e > 0 {
        if e & 1 == 1 {
            acc = dec_mul(ctx, vm, &acc, &base)?;
        }
        e >>= 1;
        if e > 0 {
            base = dec_mul(ctx, vm, &base, &base)?;
        }
    }
    if n < 0 {
        return Ok(dec_div(ctx, vm, &dec_from_i64(1), &acc)?);
    }
    Ok(acc)
}
/// Round a decimal to the nearest integer (half-even).
fn round_to_int(d: &Dec) -> Dec {
    if d.is_special() || d.is_zero() || d.exp >= 0 {
        return d.clone();
    }
    let drop = (-d.exp) as u32;
    let divisor = BigInt::from(10).pow(drop);
    let q = &d.coeff / &divisor;
    let r = &d.coeff % &divisor;
    let mut coeff = q;
    let twice = &r * 2u32;
    if twice > divisor || (twice == divisor && (&coeff % 2u32) == BigInt::from(1)) {
        coeff += 1u32;
    }
    Dec { sign: d.sign, coeff, exp: 0, special: Special::Finite }
}


/// FMA NaN selection (decTest fmax0900-0908): a signaling NaN outranks a
/// quiet NaN; within the same class the first operand in x, y, z order wins.
/// The selected sNaN signals InvalidOperation (trap -> raise, else quieted).
/// Returns None when no operand is NaN and the arithmetic should proceed.
pub(crate) fn fma_special(
    vm: &VirtualMachine,
    ctx: &Ctx,
    x: &Dec,
    y: &Dec,
    z: &Dec,
) -> PyResult<Option<Dec>> {
    // The 0 * Infinity product invalidates the whole fma before any NaN
    // selection (decTest fmax0809: fma -Inf 0 NaN -> NaN Invalid_operation,
    // a generic NaN - the payload does not come from z).
    let x_inf = x.special == Special::Infinity;
    let y_inf = y.special == Special::Infinity;
    let x_zero = !x_inf && x.special == Special::Finite && x.coeff == BigInt::from(0);
    let y_zero = !y_inf && y.special == Special::Finite && y.coeff == BigInt::from(0);
    if (x_inf && y_zero) || (y_inf && x_zero) {
        ctx.set_flag(vm, "InvalidOperation")?;
        if ctx.trap(vm, "InvalidOperation")? {
            return Err(new_decimal_error(vm, "InvalidOperation", "0 * Infinity"));
        }
        return Ok(Some(Dec {
            sign: false,
            coeff: BigInt::from(0),
            exp: 0,
            special: Special::NaN,
        }));
    }
    for d in [x, y, z] {
        if d.special == Special::SignalingNaN {
            ctx.set_flag(vm, "InvalidOperation")?;
            if ctx.trap(vm, "InvalidOperation")? {
                return Err(new_decimal_error(vm, "InvalidOperation", "operation with sNaN"));
            }
            let mut n = d.clone();
            n.special = Special::NaN;
            // Truncate the payload to context precision keeping the LOW digits
            // (fmax363000: fma 1 1 sNaN1234567890 at prec 5 -> NaN67890).
            let digits = n.coeff_str();
            let keep = (ctx.prec as usize).max(1);
            if digits.len() > keep {
                n.coeff = digits[digits.len() - keep..]
                    .parse::<BigInt>()
                    .unwrap_or_else(|_| BigInt::from(0));
            }
            return Ok(Some(n));
        }
    }
    for d in [x, y, z] {
        if d.special == Special::NaN {
            return Ok(Some(d.clone()));
        }
    }
    Ok(None)
}

/// Add two finite decimals (context applied).

/// Compare a*10**ea with b*10**eb for non-negative integers a, b whose
/// decimal strings are bounded by their operands. Decision by leading
/// position first; equal leads compare the aligned digit strings, with a
/// tie broken by whichever spans further down with a nonzero digit.
fn cmp_scaled(a: &BigInt, ea: isize, b: &BigInt, eb: isize) -> std::cmp::Ordering {
    let zero = BigInt::from(0);
    let a_zero = a == &zero;
    let b_zero = b == &zero;
    match (a_zero, b_zero) {
        (true, true) => return std::cmp::Ordering::Equal,
        (true, false) => return std::cmp::Ordering::Less,
        (false, true) => return std::cmp::Ordering::Greater,
        (false, false) => {}
    }
    let sa = a.to_string();
    let sb = b.to_string();
    let la = ea + sa.len() as isize - 1;
    let lb = eb + sb.len() as isize - 1;
    match la.cmp(&lb) {
        std::cmp::Ordering::Equal => {
            // Equal leads: compare the overlapping digits (the shorter
            // span), then break the tie by whichever extends further down
            // with a nonzero digit. No padding: the spans can differ by
            // millions of positions for huge exponents.
            let n = sa.len().min(sb.len());
            match sa[..n].cmp(&sb[..n]) {
                std::cmp::Ordering::Equal => {
                    let a_ext = sa.len() > n && sa[n..].chars().any(|c| c != '0');
                    let b_ext = sb.len() > n && sb[n..].chars().any(|c| c != '0');
                    if a_ext {
                        std::cmp::Ordering::Greater
                    } else if b_ext {
                        std::cmp::Ordering::Less
                    } else {
                        std::cmp::Ordering::Equal
                    }
                }
                ord => ord,
            }
        }
        ord => ord,
    }
}

pub(crate) fn dec_add(ctx: &Ctx, vm: &VirtualMachine, a: &Dec, b: &Dec) -> PyResult<Dec> {
    // Special propagation.
    if a.is_special() || b.is_special() {
        return special_binary(vm, ctx, a, b, "add");
    }
    // A zero operand contributes nothing but can pull the exponent down:
    // add(0.00, 5) -> 5.00 and add(5, 0E-999999999) -> 5.000...(prec digits,
    // Rounded). The result's exponent is min(e_a, e_b) clamped so the
    // coefficient still fits the precision; only the clamping (an unreachable
    // ideal exponent) signals Rounded - the padded zeros are exact, so never
    // Inexact. Never align literally: add(1, 0E+999999999) would otherwise
    // build a billion-digit BigInt below (rmnx270 hung test_remainderNear).
    if a.is_zero() || b.is_zero() {
        let min_e = a.exp.min(b.exp);
        if a.is_zero() && b.is_zero() {
            let sign = if ctx.rounding == Rounding::Floor {
                a.sign || b.sign
            } else {
                a.sign && b.sign
            };
            return apply_context(vm, ctx, Dec {
                sign,
                coeff: BigInt::from(0),
                exp: min_e,
                special: Special::Finite,
            });
        }
        let keep = if a.is_zero() { b } else { a };
        let digits = keep.coeff_digits() as isize;
        // The most negative exponent the result may take: the coefficient can
        // absorb at most (prec - digits) trailing zeros (never more: an
        // over-prec operand is rounded by apply_context, not padded).
        let floor_e = keep.exp - (ctx.prec - digits).max(0);
        let new_exp = min_e.max(floor_e);
        let pad = (keep.exp - new_exp) as u32;
        let coeff = &keep.coeff * BigInt::from(10).pow(pad);
        if new_exp > min_e {
            ctx.set_flag(vm, "Rounded")?;
            if ctx.trap(vm, "Rounded")? {
                return Err(new_decimal_error(vm, "Rounded", "rounded result"));
            }
        }
        return apply_context(vm, ctx, Dec {
            sign: keep.sign,
            coeff,
            exp: new_exp,
            special: Special::Finite,
        });
    }
    // Aligning exponents literally would build 10**(exp difference), which for
    // operands like 1e-9999999 is a ten-million-digit BigInt and takes seconds.
    // When one operand's leading digit sits more than a context precision
    // below the other's, it cannot affect any digit that survives rounding, so
    // the result is the dominant operand alone (and the operation is inexact).
    //
    // EXCEPTION: for opposite-sign operands whose gap is small enough that the
    // subtraction borrows into the kept region (addx444: 1 + (-77e-11) at prec
    // 9 must be 0.999999999, not 1.00000000), the exact path is bounded
    // (10**gap ≤ 10**(prec + digits_of_small)) and gives the correct answer.
    // Only use the fast path when the exact path would be unbounded.
    let lead_a = a.exp + a.coeff_digits() as isize - 1;
    let lead_b = b.exp + b.coeff_digits() as isize - 1;
    if !a.is_zero() && !b.is_zero() {
        let (dominant, small, gap) = if lead_a >= lead_b {
            (a, b, lead_a - lead_b)
        } else {
            (b, a, lead_b - lead_a)
        };
        let same_sign = dominant.sign == small.sign;
        let small_digits = small.coeff_digits() as isize;
        // Fast path: gap > prec AND (same-sign, or the exact path is
        // unbounded so the borrow can't reach the kept region).
        // EXCEPTION: for opposite-sign with Down/Floor AND the dominant
        // needs padding (digits < prec), the padded result is 100.000...0
        // but the correct answer is 99.999...9 (the borrow). The mode-aware
        // adj only handles the full-precision case; for padded results with
        // Down/Floor, fall through to the exact path (ddadd71100: add 1e+2
        // -1e-383 under ROUND_DOWN -> 99.99999999999999, not 100.0000000000000).
        let dom_digits = dominant.coeff_digits() as isize;
        let skip_for_mode = !same_sign && dom_digits < ctx.prec
            && matches!(ctx.rounding, Rounding::Down | Rounding::Floor);
        if gap > ctx.prec && (same_sign || gap > ctx.prec + small_digits) && !skip_for_mode {
            ctx.set_flag(vm, "Inexact")?;
            if ctx.trap(vm, "Inexact")? {
                return Err(new_decimal_error(vm, "Inexact", "inexact result"));
            }
            // For opposite-sign operands at the Emin boundary, the exact
            // result dips below Emin (subnormal) even though the rounded
            // result rounds back to the dominant (normal). CPython signals
            // Underflow+Subnormal based on the exact value (addx579:
            // subtract 1E-383 1E-401 -> ... Underflow Inexact Subnormal
            // Rounded). The dominant's adjusted exponent == Emin means the
            // exact value is just below.
            if !same_sign {
                let dom_adj = dominant.exp + dominant.coeff_digits() as isize - 1;
                // Only signal Subnormal+Underflow when the borrow changes the
                // leading digit (the dominant is a power of 10 at the Emin
                // boundary: 1E-383 - 1E-401 -> exact dips below Emin).
                // For non-power-of-10 dominants the borrow changes a lower
                // digit and the adjusted stays at Emin (addx1132: 1.000001E-95
                // - 1e-200 -> 1.000000E-95, no Subnormal).
                let ds = dominant.coeff.to_string();
                let is_pow10 = ds.starts_with('1') && ds[1..].chars().all(|c| c == '0');
                if dom_adj == ctx.emin && is_pow10 {
                    ctx.set_flag(vm, "Subnormal")?;
                    if ctx.trap(vm, "Subnormal")? {
                        return Err(ctx_trap_exc(vm, "Subnormal", "subnormal"));
                    }
                    ctx.set_flag(vm, "Underflow")?;
                    if ctx.trap(vm, "Underflow")? {
                        return Err(ctx_trap_exc(vm, "Underflow", "underflow"));
                    }
                }
            }
            // CPython pads the survivor out to the context precision, because the
            // dropped operand extended the exact result below it:
            // 1 + 1e-9999999 -> 1.000000000000000000000000000.
            let digits = dominant.coeff_digits() as isize;
            if digits < ctx.prec {
                // For opposite-sign with modes that round toward the borrow
                // (Down always; Floor for positive; Ceiling for negative),
                // the padded result (100.000...0) is wrong — the correct
                // answer is all 9s (99.999...9). Compute the borrow directly:
                // (10^prec - 1) at exp = dom_adjusted - 1 - prec + 1.
                if !same_sign {
                    let toward_borrow = match ctx.rounding {
                        Rounding::Down => true,
                        Rounding::Floor => !dominant.sign,
                        Rounding::Ceiling => dominant.sign,
                        _ => false,
                    };
                    if toward_borrow {
                        let dom_adj = dominant.exp + digits - 1;
                        let borrow_coeff = BigInt::from(10).pow(ctx.prec as u32) - BigInt::from(1);
                        let borrow_exp = dom_adj - ctx.prec as isize;
                        ctx.set_flag(vm, "Rounded")?;
                        if ctx.trap(vm, "Rounded")? {
                            return Err(new_decimal_error(vm, "Rounded", "rounded result"));
                        }
                        return apply_context(vm, ctx, Dec {
                            sign: dominant.sign,
                            coeff: borrow_coeff,
                            exp: borrow_exp,
                            special: Special::Finite,
                        });
                    }
                }
                let extra = (ctx.prec - digits) as u32;
                let padded = Dec {
                    sign: dominant.sign,
                    coeff: &dominant.coeff * BigInt::from(10).pow(extra),
                    exp: dominant.exp - extra as isize,
                    special: Special::Finite,
                };
                ctx.set_flag(vm, "Rounded")?;
                if ctx.trap(vm, "Rounded")? {
                    return Err(new_decimal_error(vm, "Rounded", "rounded result"));
                }
                // Mode-aware rounding for the padded case: same-sign with
                // Up/Ceiling rounds the padded value up (+1 in the last
                // digit, addx62072: add 12345678 1E-10 ROUND_UP ->
                // 12345678.00000001, not 12345678.00000000).
                let pad_adj: i32 = if same_sign {
                    match ctx.rounding {
                        Rounding::Up => 1,
                        Rounding::Ceiling if !dominant.sign => 1,
                        Rounding::Floor if dominant.sign => 1,
                        _ => 0,
                    }
                } else {
                    0
                };
                if pad_adj != 0 {
                    return apply_context(vm, ctx, Dec {
                        sign: padded.sign,
                        coeff: &padded.coeff + BigInt::from(pad_adj as u32),
                        exp: padded.exp,
                        special: Special::Finite,
                    });
                }
                return apply_context(vm, ctx, padded);
            }
            ctx.set_flag(vm, "Rounded")?;
            if ctx.trap(vm, "Rounded")? {
                return Err(new_decimal_error(vm, "Rounded", "rounded result"));
            }
            // Mode-aware rounding: the dominant has prec digits and the small
            // operand is entirely below the precision (|delta| << 0.5 ulp
            // since gap > prec + small_digits). For same-sign, ceiling/up
            // round away from zero (+1); for opposite-sign borrow, down/floor
            // round toward zero (-1). Half modes round to nearest = dominant.
            let adj: i32 = if same_sign {
                match ctx.rounding {
                    Rounding::Up => 1,
                    Rounding::Ceiling if !dominant.sign => 1,
                    Rounding::Floor if dominant.sign => 1,
                    _ => 0,
                }
            } else {
                // For opposite-sign with Down/Floor(positive)/Ceiling(negative)
                // and a power-of-10 dominant, the borrow is 10^prec-1 (all 9s)
                // at a lower exponent, not dominant-1 which gives the wrong digit
                // count (addx1131: 1.000000E-94 - 1e-200 -> 9.999999E-95, not
                // 9.99999E-95 from 1000000-1=999999).
                let s = dominant.coeff.to_string();
                let is_pow10 = s.starts_with('1') && s[1..].chars().all(|c| c == '0');
                let toward_borrow = matches!(ctx.rounding, Rounding::Down)
                    || (matches!(ctx.rounding, Rounding::Floor) && !dominant.sign)
                    || (matches!(ctx.rounding, Rounding::Ceiling) && dominant.sign);
                if is_pow10 && toward_borrow {
                    let dom_adj = dominant.exp + dominant.coeff_digits() as isize - 1;
                    let borrow_coeff = BigInt::from(10).pow(ctx.prec as u32) - BigInt::from(1);
                    let borrow_exp = dom_adj - ctx.prec as isize;
                    return apply_context(vm, ctx, Dec {
                        sign: dominant.sign,
                        coeff: borrow_coeff,
                        exp: borrow_exp,
                        special: Special::Finite,
                    });
                }
                match ctx.rounding {
                    Rounding::Down => -1,
                    Rounding::Floor if !dominant.sign => -1,
                    Rounding::Ceiling if dominant.sign => -1,
                    _ => 0,
                }
            };
            if adj != 0 {
                let coeff = if adj > 0 {
                    &dominant.coeff + BigInt::from(adj as u32)
                } else {
                    &dominant.coeff - BigInt::from((-adj) as u32)
                };
                return apply_context(vm, ctx, Dec {
                    sign: dominant.sign,
                    coeff,
                    exp: dominant.exp,
                    special: Special::Finite,
                });
            }
            return apply_context(vm, ctx, dominant.clone());
        }
    }
    let e = a.exp.min(b.exp);
    let pa = BigInt::from(10).pow((a.exp - e) as u32);
    let pb = BigInt::from(10).pow((b.exp - e) as u32);
    let ca = &a.coeff * &pa;
    let cb = &b.coeff * &pb;
    let va = if a.sign { -&ca } else { ca.clone() };
    let vb = if b.sign { -&cb } else { cb.clone() };
    let sum = va + vb;
    // Zero sign (decTest 3.14 / IEEE 854): under ROUND_FLOOR a single
    // negative-zero operand keeps the result negative (addx1621: add(-0,
    // 0E-19) -> -0E-19; addx1625: add(-0E-400, 0E-19) -> -0E-398 Clamped).
    // Under any other rounding mode only two negative zeros keep the sign
    // (addx61501: add(-0, 0E-19) -> 0E-19; addx61503: add(-0, -0E-19) ->
    // -0E-19). Both positive zeros -> +0.
    let sign = if sum == BigInt::from(0) {
        if ctx.rounding == Rounding::Floor {
            // ROUND_FLOOR: a zero result from any negative contribution is
            // negative (add(1E-401,-1E-401) -> -0E-398; add(-0,0E-19) ->
            // -0E-19). The sum is zero only when operands cancel.
            a.sign || b.sign
        } else {
            // Other modes: only two negative zeros keep the sign
            // (addx61501: add(-0, 0E-19) -> 0E-19; addx61503: add(-0, -0E-19)
            // -> -0E-19); cancelling nonzero operands give +0
            // (add(1,-1) -> 0 under ROUND_HALF_UP).
            a.is_zero() && a.sign && b.is_zero() && b.sign
        }
    } else {
        sum.sign() == Sign::Minus
    };
    let coeff = sum.abs();
    apply_context(vm, ctx, Dec { sign, coeff, exp: e, special: Special::Finite })
}

/// Subtract (a - b).
pub(crate) fn dec_sub(ctx: &Ctx, vm: &VirtualMachine, a: &Dec, b: &Dec) -> PyResult<Dec> {
    // A NaN operand keeps its original sign through the negation: CPython
    // subtract(-1,-sNaN) -> -NaN (the sNaN's negative sign survives), while
    // a finite b is negated normally.
    let neg = if b.special == Special::NaN || b.special == Special::SignalingNaN {
        b.clone()
    } else {
        Dec { sign: !b.sign, coeff: b.coeff.clone(), exp: b.exp, special: b.special }
    };
    dec_add(ctx, vm, a, &neg)
}

/// Multiply.
pub(crate) fn dec_mul(ctx: &Ctx, vm: &VirtualMachine, a: &Dec, b: &Dec) -> PyResult<Dec> {
    if a.is_special() || b.is_special() {
        return special_binary(vm, ctx, a, b, "mul");
    }
    let coeff = &a.coeff * &b.coeff;
    let exp = a.exp + b.exp;
    let sign = a.sign ^ b.sign;
    apply_context(vm, ctx, Dec { sign, coeff, exp, special: Special::Finite })
}

/// Special-value handling for binary ops, approximating IEEE/General Decimal.
pub(crate) fn special_binary(
    vm: &VirtualMachine,
    ctx: &Ctx,
    a: &Dec,
    b: &Dec,
    op: &str,
) -> PyResult<Dec> {
    let _ = op;
    // NaN dominates. Result sign follows the first operand a; payload comes
    // from a signaling NaN (a's if present, else b's), then a's quiet NaN
    // payload (CPython: subtract(-Inf,-sNaN) -> -NaN; subtract(1000,sNaN) ->
    // NaN; subtract(sNaN,1000) -> NaN).
    if a.special == Special::NaN || a.special == Special::SignalingNaN
        || b.special == Special::NaN || b.special == Special::SignalingNaN
    {
        let signaling = a.special == Special::SignalingNaN || b.special == Special::SignalingNaN;
        if signaling {
            ctx.set_flag(vm, "InvalidOperation")?;
            if ctx.trap(vm, "InvalidOperation")? {
                return Err(new_decimal_error(
                    vm,
                    "InvalidOperation",
                    "operation with sNaN",
                ));
            }
        }
        // Payload from the sNaN operand (a's if present, else b's), then a's
        // quiet NaN; the result sign follows the first operand if it is a
        // NaN, otherwise the second operand's sign (CPython:
        // divide(-Inf,-NaN90) -> -NaN90; divide(NaN,-NaN) -> NaN;
        // divide(Inf,-sNaN90) -> -NaN90; subtract(-1,-sNaN) -> -NaN).
        let mut n = if a.special == Special::SignalingNaN {
            a.clone()
        } else if b.special == Special::SignalingNaN {
            b.clone()
        } else if a.special == Special::NaN {
            a.clone()
        } else {
            b.clone()
        };
        n.sign = if a.special == Special::NaN || a.special == Special::SignalingNaN {
            a.sign
        } else {
            b.sign
        };
        n.special = Special::NaN;
        // Payload truncated to context precision (sNaN123456789 -> NaN56789
        // at prec 6, addx62100).
        let digits = n.coeff_str();
        let keep = (ctx.prec as usize).max(1);
        if digits.len() > keep {
            n.coeff = digits[digits.len() - keep..]
                .parse::<BigInt>()
                .unwrap_or_else(|_| BigInt::from(0));
        }
        return Ok(n);
    }
    let inf_a = a.special == Special::Infinity;
    let inf_b = b.special == Special::Infinity;
    match op {
        "add" => {
            if inf_a && inf_b {
                // Inf + Inf: same sign -> that Infinity; opposite signs ->
                // InvalidOperation + NaN (addx6780: add(-Inf,-Inf) -> -Infinity).
                if a.sign == b.sign {
                    return Ok(Dec { sign: a.sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
                }
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "Inf + -Inf"));
                }
                return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
            }
            if inf_a {
                return Ok(Dec { sign: a.sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
            }
            if inf_b {
                return Ok(Dec { sign: b.sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
            }
        }
        "mul" => {
            if inf_a && inf_b {
                return Ok(Dec { sign: a.sign ^ b.sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
            }
            if inf_a {
                // Inf * 0 -> InvalidOperation + NaN; otherwise sign XOR.
                if b.is_zero() {
                    ctx.set_flag(vm, "InvalidOperation")?;
                    if ctx.trap(vm, "InvalidOperation")? {
                        return Err(new_decimal_error(vm, "InvalidOperation", "Inf * 0"));
                    }
                    return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
                }
                return Ok(Dec { sign: a.sign ^ b.sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
            }
            if inf_b {
                if a.is_zero() {
                    ctx.set_flag(vm, "InvalidOperation")?;
                    if ctx.trap(vm, "InvalidOperation")? {
                        return Err(new_decimal_error(vm, "InvalidOperation", "0 * Inf"));
                    }
                    return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
                }
                return Ok(Dec { sign: a.sign ^ b.sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
            }
        }
        "div" => {
            if inf_a && inf_b {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "Inf / Inf"));
                }
                return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
            }
            if inf_a {
                return Ok(Dec { sign: a.sign ^ b.sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
            }
            if inf_b {
                // finite / Inf -> zero at the smallest exponent with the
                // sign of the division (divide(1, Inf) -> 0E-398 Clamped at
                // Emin=-383/prec=16); Clamped is signaled.
                let etiny = ctx.emin - (ctx.prec as isize - 1);
                ctx.set_flag(vm, "Clamped")?;
                if ctx.trap(vm, "Clamped")? {
                    return Err(ctx_trap_exc(vm, "Clamped", "clamped"));
                }
                return Ok(Dec {
                    sign: a.sign ^ b.sign,
                    coeff: BigInt::from(0),
                    exp: etiny,
                    special: Special::Finite,
                });
            }
        }
        "mod" => {
            // remainder/mod: a finite operand with an infinite divisor is the
            // result (remainder(1, Inf) -> 1); an infinite operand is
            // InvalidOperation (remainder(Inf, 2) raises).
            if inf_b && !inf_a {
                return Ok(a.clone());
            }
            if inf_a || inf_b {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "Inf floor/mod"));
                }
                return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
            }
        }
        "floordiv" => {
            // floordiv: finite // Inf -> zero with the quotient sign
            // (divideint(-1000, Inf) -> -0); ±Inf // finite -> ±Infinity
            // (divideint(-Inf, 1000) -> -Infinity); Inf // Inf -> NaN.
            if inf_b && !inf_a {
                return Ok(Dec { sign: a.sign ^ b.sign, coeff: BigInt::from(0), exp: 0, special: Special::Finite });
            }
            if inf_a && !inf_b {
                return Ok(Dec { sign: a.sign ^ b.sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
            }
            if inf_a || inf_b {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "Inf // Inf"));
                }
                return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
            }
        }
        _ => {
            // other ops with any Infinity -> InvalidOperation + NaN.
            if inf_a || inf_b {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "Inf floor/mod"));
                }
                return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
            }
        }
    }
    Ok(Dec::zero())
}

/// True division with context precision (long division to prec+1 digits).
pub(crate) fn dec_div(ctx: &Ctx, vm: &VirtualMachine, a: &Dec, b: &Dec) -> PyResult<Dec> {
    // Specials first: a NaN/Infinity operand wins over division-by-zero
    // (divide(NaN, 0) -> NaN; divide(Inf, 0) -> Infinity via special_binary).
    if a.is_special() || b.is_special() {
        return special_binary(vm, ctx, a, b, "div");
    }
    if b.is_zero() {
        if a.is_zero() {
            // 0 / 0 -> Division_undefined (InvalidOperation) + NaN
            // (divx736: divide(-0,-0) -> NaN Division_undefined).
            ctx.set_flag(vm, "InvalidOperation")?;
            if ctx.trap(vm, "InvalidOperation")? {
                return Err(new_decimal_error(vm, "InvalidOperation", "division undefined"));
            }
            return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
        }
        ctx.set_flag(vm, "DivisionByZero")?;
        if ctx.trap(vm, "DivisionByZero")? {
            return Err(ctx_trap_exc(vm, "DivisionByZero", "division by zero"));
        }
        return Ok(Dec {
            sign: a.sign ^ b.sign,
            coeff: BigInt::from(0),
            exp: 0,
            special: Special::Infinity,
        });
    }
    // a / b = (ca * 10^ea) / (cb * 10^eb)
    //
    // An exact integer quotient is returned as-is, which keeps trailing
    // zeros like CPython: 2.0/2 -> 1.0.
    let prec = ctx.prec as usize;
    let num = a.coeff.clone();
    let den = b.coeff.clone();
    let ip = &num / &den;
    let (coeff, exp) = if &num % &den == BigInt::from(0) {
        (ip, a.exp - b.exp)
    } else {
        // Non-terminating quotient: generate prec + 2 *significant* digits with
        // a single BigInt division rather than one digit per long-division
        // step, which cost a full multiply-add per digit. The guard digits
        // matter: 5/11 = 0.45454545... at prec 7 would otherwise truncate to
        // 4545454|5 (a false half-tie -> 4545454); with the 9th digit 4 the
        // discarded part 54 > 50 rounds up (divx1051 -> 0.4545455).
        //
        // The scaling must cover significant digits, not fractional ones: for
        // a quotient below 1, `num * 10^(prec+2) / den` yields prec+2
        // fractional places but only ~prec significant digits, which would lose
        // one digit against the context (1/4.125 -> 27 digits, not 28). The
        // extra digit also breaks an exact half-tie in the guard place:
        // 1/7 at prec 28 ends ...428|5 and must round up, not to even.
        let num_digits = coeff_digits_of(&num);
        let den_digits = coeff_digits_of(&den);
        let shift = (prec + 2 + den_digits).saturating_sub(num_digits);
        let scaled = &num * BigInt::from(10).pow(shift as u32);
        let quotient = &scaled / &den;
        let remainder = &scaled % &den;
        let exp = a.exp - b.exp - shift as isize;
        if remainder == BigInt::from(0) {
            // The quotient terminated inside the guard digits, so the zeros
            // the scaling introduced are not part of the value: strip them to
            // get CPython's canonical form (1/2 -> 0.5, not 0.5000...).
            let mut coeff = quotient;
            let mut exp = exp;
            let ten = BigInt::from(10);
            while &coeff % &ten == BigInt::from(0) {
                coeff /= &ten;
                exp += 1;
            }
            (coeff, exp)
        } else {
            (quotient, exp)
        }
    };
    let sign = a.sign ^ b.sign;
    apply_context(vm, ctx, Dec { sign, coeff, exp, special: Special::Finite })
}

/// Map a Unicode decimal digit character to its 0-9 value. Covers the common
/// decimal digit blocks; returns None for non-digit characters.
fn unicode_decimal_digit(c: char) -> Option<u32> {
    let cp = c as u32;
    let blocks: &[(u32, u32)] = &[
        (0x0030, 0x0039), // ASCII
        (0x0660, 0x0669), // Arabic-Indic
        (0x06F0, 0x06F9), // Extended Arabic-Indic
        (0x0966, 0x096F), // Devanagari
        (0x09E6, 0x09EF), // Bengali
        (0x0A66, 0x0A6F), // Gurmukhi
        (0x0AE6, 0x0AEF), // Gujarati
        (0x0B66, 0x0B6F), // Oriya
        (0x0BE6, 0x0BEF), // Tamil
        (0x0C66, 0x0C6F), // Telugu
        (0x0CE6, 0x0CEF), // Kannada
        (0x0D66, 0x0D6F), // Malayalam
        (0x0E50, 0x0E59), // Thai
        (0x0ED0, 0x0ED9), // Lao
        (0x0F20, 0x0F29), // Tibetan
        (0x1040, 0x1049), // Myanmar
        (0x17E0, 0x17E9), // Khmer
        (0x1810, 0x1819), // Mongolian
        (0xFF10, 0xFF19), // Fullwidth
    ];
    for (start, end) in blocks {
        if cp >= *start && cp <= *end {
            return Some(cp - *start);
        }
    }
    None
}

/// Parse a decimal string. Returns Dec.
pub(crate) fn parse_decimal(s: &str) -> Result<Dec, String> {
    let trimmed = s.trim();
    // CPython raises InvalidOperation for an empty or whitespace-only
    // literal (ConversionSyntax); only an actual value string is valid.
    if trimmed.is_empty() {
        return Err("invalid literal".to_string());
    }
    let mut chars = trimmed.chars().peekable();
    let mut sign = false;
    if let Some(&c) = chars.peek() {
        if c == '+' { chars.next(); }
        else if c == '-' { sign = true; chars.next(); }
    }
    let rest: String = chars.collect();
    // CPython permits underscores as digit separators in literals
    // (Decimal('1_3.3e4_0') == Decimal('1.33E+41')); strip them.
    let rest: String = rest.chars().filter(|&c| c != '_').collect();
    // Normalize Unicode decimal digits (Arabic-Indic \u0660-\u0669, extended
    // \u06f0-\u06f9, Devanagari \u0966-\u096f, fullwidth \uff10-\uff19,
    // etc.) to ASCII. Rust's char::to_digit only covers ASCII, so map by
    // decimal-digit category manually.
    let rest: String = rest
        .chars()
        .map(|c| {
            if c.is_ascii() {
                c
            } else {
                let d = unicode_decimal_digit(c);
                match d {
                    Some(d) => char::from(b'0' + d as u8),
                    None => c,
                }
            }
        })
        .collect();
    let lower = rest.to_ascii_lowercase();
    if lower == "infinity" || lower == "inf" {
        return Ok(Dec { sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
    }
    // NaN / sNaN with optional diagnostic digits (e.g. nan123, snan042).
    // CPython keeps the trailing digits as the NaN payload (visible in
    // as_tuple()).
    if lower == "nan"
        || (lower.starts_with("nan") && lower[3..].chars().all(|c| c.is_ascii_digit()))
    {
        let payload: BigInt = if lower.len() > 3 {
            lower[3..].parse().unwrap_or_else(|_| BigInt::from(0))
        } else {
            BigInt::from(0)
        };
        return Ok(Dec { sign, coeff: payload, exp: 0, special: Special::NaN });
    }
    if lower == "snan"
        || (lower.starts_with("snan") && lower[4..].chars().all(|c| c.is_ascii_digit()))
    {
        let payload: BigInt = if lower.len() > 4 {
            lower[4..].parse().unwrap_or_else(|_| BigInt::from(0))
        } else {
            BigInt::from(0)
        };
        return Ok(Dec { sign, coeff: payload, exp: 0, special: Special::SignalingNaN });
    }
    // split exponent
    let (mantissa, exp_part) = match rest.find(['e', 'E']) {
        Some(idx) => (&rest[..idx], Some(&rest[idx + 1..])),
        None => (rest.as_str(), None),
    };
    let exp: isize = match exp_part {
        Some(e) => e.trim().parse::<isize>().map_err(|_| "invalid exponent".to_string())?,
        None => 0,
    };
    // CPython raises InvalidOperation when the exponent exceeds the
    // representable range: upper bound MAX_EMAX (999999999999999999),
    // lower bound MIN_ETINY (-1999999999999999997). Clamping outside is
    // impossible.
    if exp > 999_999_999_999_999_999 || exp < -1_999_999_999_999_999_997 {
        return Err("exponent out of range".to_string());
    }
    // split mantissa on '.'
    let (int_part, frac_part) = match mantissa.find('.') {
        Some(idx) => (&mantissa[..idx], &mantissa[idx + 1..]),
        None => (mantissa, ""),
    };
    // No digits at all ('.', '+.e+1') is a ConversionSyntax error, not 0.
    if int_part.is_empty() && frac_part.is_empty() {
        return Err("invalid literal".to_string());
    }
    if !int_part.chars().all(|c| c.is_ascii_digit()) || !frac_part.chars().all(|c| c.is_ascii_digit())
    {
        return Err("invalid literal".to_string());
    }
    let all_digits = format!("{}{}", int_part, frac_part);
    let coeff: BigInt = if all_digits.is_empty() {
        BigInt::from(0)
    } else {
        all_digits.parse::<BigInt>().map_err(|_| "invalid literal".to_string())?
    };
    let exp = exp - frac_part.len() as isize;
    Ok(Dec { sign, coeff, exp, special: Special::Finite })
}

pub(crate) use crate::vm::builtins::PyStr;

/// Build a Decimal from an int.
pub(crate) fn dec_from_int(vm: &VirtualMachine, i: &PyInt) -> Dec {
    let v = i.as_bigint();
    let sign = v.sign() == Sign::Minus;
    Dec { sign, coeff: v.abs(), exp: 0, special: Special::Finite }
}

/// Build a Decimal from a float (shortest repr round-trip via repr()).
pub(crate) fn dec_from_float(f: f64) -> Dec {
    if f.is_nan() {
        let sign = f.is_sign_negative();
        return Dec { sign, coeff: BigInt::from(0), exp: 0, special: Special::NaN };
    }
    if f.is_infinite() {
        return Dec { sign: f.is_sign_negative(), coeff: BigInt::from(0), exp: 0, special: Special::Infinity };
    }
    if f == 0.0 {
        return Dec { sign: f.is_sign_negative(), coeff: BigInt::from(0), exp: 0, special: Special::Finite };
    }
    // Exact binary → decimal expansion: value = m * 2^e.
    let bits = f.to_bits();
    let sign = (bits >> 63) != 0;
    let exp_bits = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & 0x000f_ffff_ffff_ffff;
    let (m, e) = if exp_bits == 0 {
        // subnormal
        (BigInt::from(frac), -1074)
    } else {
        (BigInt::from(frac | 0x0010_0000_0000_0000), exp_bits - 1023 - 52)
    };
    if e >= 0 {
        let coeff = m * BigInt::from(2).pow(e as u32);
        Dec { sign, coeff, exp: 0, special: Special::Finite }
    } else {
        let k = (-e) as u32;
        let pow5 = BigInt::from(5).pow(k);
        let mut coeff = m * &pow5;
        let mut exp = -(k as isize);
        // strip trailing zeros introduced by the 5^k expansion
        while coeff != BigInt::from(0) && (&coeff % BigInt::from(10)) == BigInt::from(0) {
            coeff /= BigInt::from(10);
            exp += 1;
        }
        Dec { sign, coeff, exp, special: Special::Finite }
    }
}

/// quantize self to exponent `exp` using context rounding.
pub(crate) fn quantize(vm: &VirtualMachine, ctx: &Ctx, d: &Dec, exp: isize) -> PyResult<Dec> {
    if d.is_special() {
        return Ok(d.clone());
    }
    // CPython quantize checks (in order):
    //  1. target exponent within [Etiny, Emax]
    //  2. self_adjusted <= Emax
    //  3. self_adjusted - exp + 1 <= prec  ("too many digits")
    let etiny = ctx.emin - (ctx.prec as isize - 1);
    let digits_now = coeff_digits_of(&d.coeff) as isize;
    let adj_before = d.exp + digits_now - 1;
    // A zero coefficient may be quantized to any exponent within the target
    // exponent range [Etiny, Emax]; targets outside it are InvalidOperation
    // (quantize(0, 1e-999) -> 0E-999 when Etiny <= -999; quantize(0,
    // 1e-2000000000) -> NaN when the target is below Etiny).
    // A zero coefficient may be quantized to any exponent within [Etiny, Emax]
    // — the too-many-digits / self-adjusted checks do not apply (quantize(0,
    // 1e-999) -> 0E-999; quantize(0, 1e-2000000000) -> NaN when below Etiny).
    let invalid = if d.coeff == BigInt::from(0) {
        exp < etiny || exp > ctx.emax
    } else {
        exp < etiny || exp > ctx.emax
            || adj_before > ctx.emax
            || (adj_before - exp + 1) > ctx.prec
    };
    if invalid {
        ctx.set_flag(vm, "InvalidOperation")?;
        if ctx.trap(vm, "InvalidOperation")? {
            return Err(new_decimal_error(vm, "InvalidOperation", "quantize result out of range"));
        }
        return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
    }
    let shift = exp - d.exp;
    let mut coeff = d.coeff.clone();
    let mut inexact = false;
    if shift > 0 {
        let pow = BigInt::from(10).pow(shift as u32);
        let (q, r) = (&coeff / &pow, &coeff % &pow);
        let two = BigInt::from(2);
        let round_up = match ctx.rounding {
            Rounding::Down => false,
            // Directed rounding only moves the result when the discarded
            // digits are non-zero; an all-zero remainder is already exact.
            Rounding::Up => r != BigInt::from(0),
            Rounding::Ceiling => !d.sign && r != BigInt::from(0),
            Rounding::Floor => d.sign && r != BigInt::from(0),
            Rounding::HalfDown => (&r * &two) > pow,
            Rounding::HalfUp => (&r * &two) >= pow,
            Rounding::HalfEven => {
                if (&r * &two) > pow { true }
                else if (&r * &two) < pow { false }
                else { (&q % &two) == BigInt::from(1) }
            }
            Rounding::Half05Up => {
                if r == BigInt::from(0) {
                    false
                } else {
                    let last = (&q % BigInt::from(10)) == BigInt::from(0)
                        || (&q % BigInt::from(10)) == BigInt::from(5);
                    last
                }
            }
            Rounding::Half01Up => {
                if r == BigInt::from(0) {
                    false
                } else {
                    let last = (&q % BigInt::from(10)) == BigInt::from(0)
                        || (&q % BigInt::from(10)) == BigInt::from(1);
                    last
                }
            }
        };
        inexact = r != BigInt::from(0);
        coeff = if round_up { q + BigInt::from(1) } else { q };
    } else if shift < 0 {
        coeff *= BigInt::from(10).pow((-shift) as u32);
    }
    // Any *divisive* exponent adjustment (shift > 0, digits discarded)
    // signals Rounded, even when the discarded portion is zero (CPython:
    // quantize(12345678000, 1e+3) -> 1.2345678E+10 Rounded). Padding zeros
    // (shift < 0, value unchanged: quantize(0.1, 1e-2) -> 0.10) does not.
    // Inexact is set below alongside Rounded when digits were discarded.
    let mut result = Dec { sign: d.sign, coeff, exp, special: Special::Finite };
    // CPython: quantize raises InvalidOperation when the result would have
    // more digits than the context precision ('quantize result has too many
    // digits for current context'); with the trap disabled it returns NaN.
    if coeff_digits_of(&result.coeff) as isize > ctx.prec
        || result.exp + coeff_digits_of(&result.coeff) as isize - 1 > ctx.emax
    {
        ctx.set_flag(vm, "InvalidOperation")?;
        if ctx.trap(vm, "InvalidOperation")? {
            return Err(new_decimal_error(vm, "InvalidOperation", "quantize result has too many digits for current context"));
        }
        return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
    }
    if shift > 0 && !d.is_zero() {
        // Rounded whenever digits were discarded (divisive adjustment),
        // even with a zero remainder; Inexact only when nonzero discarded.
        // A zero coefficient never signals Rounded (quantize(0, 1e+5) ->
        // 0E+5 has no flags).
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
    } else if inexact {
        // shift < 0 (padding) cannot be inexact; shift == 0 with inexact is
        // impossible, but keep the guard for safety.
        ctx.set_flag(vm, "Inexact")?;
        ctx.set_flag(vm, "Rounded")?;
    }
    // A nonzero quantized result whose adjusted exponent is below Emin is
    // subnormal (flag only; the exact exponent is preserved):
    // quantize(1E-1000, 1e-1005) -> 1.00000E-1000 Subnormal.
    if !result.is_zero()
        && (result.exp as i128) + coeff_digits_of(&result.coeff) as i128 - 1 < ctx.emin as i128
    {
        ctx.set_flag(vm, "Subnormal")?;
        if ctx.trap(vm, "Subnormal")? {
            return Err(ctx_trap_exc(vm, "Subnormal", "subnormal"));
        }
    }
    Ok(result)
}

/// Round to integer (exp 0) using the context rounding mode, WITHOUT the
/// Emax/Emin range check (CPython's Decimal.to_integral* returns the rounded
/// integer even if its adjusted exponent exceeds Emax). `exact` controls
/// whether Inexact/Rounded are signaled+trapped (to_integral_exact does,
/// to_integral/to_integral_value do not).
pub(crate) fn to_integral(vm: &VirtualMachine, ctx: &Ctx, d: &Dec, exact: bool) -> PyResult<Dec> {
    if d.is_special() {
        if d.special == Special::Infinity || d.special == Special::NaN || d.special == Special::SignalingNaN {
            ctx.set_flag(vm, "InvalidOperation")?;
            if ctx.trap(vm, "InvalidOperation")? {
                return Err(new_decimal_error(vm, "InvalidOperation", "to_integral of special"));
            }
            return Ok(Dec { sign: d.sign, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
        }
        return Ok(d.clone());
    }
    quantize_inner(vm, ctx, d, 0, exact)
}

/// quantize to `exp` using context rounding — shared core without the
/// Emax/Emin InvalidOperation check (used by to_integral).
fn quantize_inner(vm: &VirtualMachine, ctx: &Ctx, d: &Dec, exp: isize, exact: bool) -> PyResult<Dec> {
    let shift = exp - d.exp;
    let mut coeff = d.coeff.clone();
    let mut inexact = false;
    if shift > 0 {
        let pow = BigInt::from(10).pow(shift as u32);
        let (q, r) = (&coeff / &pow, &coeff % &pow);
        let two = BigInt::from(2);
        let round_up = match ctx.rounding {
            Rounding::Down => false,
            // Directed rounding only moves the result when the discarded
            // digits are non-zero; an all-zero remainder is already exact.
            Rounding::Up => r != BigInt::from(0),
            Rounding::Ceiling => !d.sign && r != BigInt::from(0),
            Rounding::Floor => d.sign && r != BigInt::from(0),
            Rounding::HalfDown => (&r * &two) > pow,
            Rounding::HalfUp => (&r * &two) >= pow,
            Rounding::HalfEven => {
                if (&r * &two) > pow { true }
                else if (&r * &two) < pow { false }
                else { (&q % &two) == BigInt::from(1) }
            }
            Rounding::Half05Up => {
                if r == BigInt::from(0) { false }
                else { (&q % BigInt::from(10)) == BigInt::from(0) || (&q % BigInt::from(10)) == BigInt::from(5) }
            }
            Rounding::Half01Up => {
                if r == BigInt::from(0) { false }
                else { (&q % BigInt::from(10)) == BigInt::from(0) || (&q % BigInt::from(10)) == BigInt::from(1) }
            }
        };
        inexact = r != BigInt::from(0);
        coeff = if round_up { q + BigInt::from(1) } else { q };
    } else if shift < 0 {
        coeff *= BigInt::from(10).pow((-shift) as u32);
    }
    let result = Dec { sign: d.sign, coeff, exp, special: Special::Finite };
    // Maintain Inexact/Rounded flags; to_integral_exact (exact=true) also
    // traps them, to_integral/to_integral_value (exact=false) do not.
    // Rounded is signaled when fractional digits were discarded even with a
    // zero remainder (to_integral_exact(-1.0) -> -1, Rounded), matching the C
    // implementation; a zero operand never signals it (to_integral_exact(
    // -0E-3) -> -0, no flags; 0.0 -> 0, no flags).
    if shift > 0 && (!d.is_zero() || inexact) {
        ctx.set_flag(vm, "Rounded")?;
        if exact && ctx.trap(vm, "Rounded")? {
            return Err(new_decimal_error(vm, "Rounded", "rounded result"));
        }
        if inexact {
            ctx.set_flag(vm, "Inexact")?;
            if exact && ctx.trap(vm, "Inexact")? {
                return Err(new_decimal_error(vm, "Inexact", "inexact result"));
            }
        }
    }
    Ok(result)
}

/// Integer exponentiation with context rounding.
pub(crate) fn dec_pow_int(ctx: &Ctx, vm: &VirtualMachine, a: &Dec, b: &Dec) -> PyResult<Dec> {
    // 0 ** 0 is Invalid_operation (decTest powx001: power 0 0 -> NaN
    // Invalid_operation; CPython raises too). The result is NaN.
    if a.is_zero() && b.is_zero() {
        ctx.set_flag(vm, "InvalidOperation")?;
        if ctx.trap(vm, "InvalidOperation")? {
            return Err(new_decimal_error(vm, "InvalidOperation", "0 ** 0 is undefined"));
        }
        return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
    }
    if b.special != Special::Finite {
        // sNaN exponent signals before the NaN base propagates (powx652:
        // power NaN sNaN -> NaN Invalid_operation).
        if b.special == Special::SignalingNaN {
            ctx.set_flag(vm, "InvalidOperation")?;
            if ctx.trap(vm, "InvalidOperation")? {
                return Err(new_decimal_error(vm, "InvalidOperation", "sNaN exponent"));
            }
            let mut n = b.clone();
            n.special = Special::NaN;
            return Ok(n);
        }
        // A NaN base propagates before the exponent's special handling
        // (powx621: power NaN -Inf -> NaN, not Infinity).
        if a.special == Special::SignalingNaN {
            ctx.set_flag(vm, "InvalidOperation")?;
            if ctx.trap(vm, "InvalidOperation")? {
                return Err(new_decimal_error(vm, "InvalidOperation", "sNaN operand"));
            }
            let mut n = a.clone();
            n.special = Special::NaN;
            return Ok(n);
        }
        if a.special == Special::NaN {
            return Ok(a.clone());
        }
        // b = +Inf: depends on |a| and a's sign.
        // b = -Inf: Inf**(-Inf) = 0 (powx580).
        if b.special == Special::Infinity && !b.sign {
            // b = +Inf
            if a.special == Special::Infinity {
                if a.sign {
                    // -Inf ** Inf = NaN (powx591)
                    ctx.set_flag(vm, "InvalidOperation")?;
                    if ctx.trap(vm, "InvalidOperation")? {
                        return Err(new_decimal_error(vm, "InvalidOperation", "-Inf ** Inf"));
                    }
                    return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
                }
                return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
            }
            if a.is_zero() {
                // 0 ** Inf = +0 (powx594: -0 ** Inf -> 0, not -0).
                return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Finite });
            }
            if a.sign {
                // negative finite ** Inf = NaN (powx590, 592, 593)
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "negative ** Inf"));
                }
                return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
            }
            // positive finite ** Inf: |a| > 1 -> Inf; |a| < 1 -> 0; |a| = 1 -> 1.
            let adj = a.exp + a.coeff_digits() as isize - 1;
            if adj > 0 {
                return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
            } else if adj < 0 {
                return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Finite });
            } else if a.coeff == BigInt::from(1) {
                // 1 ** Inf = 1 padded to context precision with Inexact+Rounded
                // (powx597: power 1 Inf -> 1.00000000 Inexact Rounded).
                let pad = (ctx.prec - 1).max(0) as u32;
                ctx.set_flag(vm, "Inexact")?;
                if ctx.trap(vm, "Inexact")? {
                    return Err(new_decimal_error(vm, "Inexact", "inexact result"));
                }
                ctx.set_flag(vm, "Rounded")?;
                if ctx.trap(vm, "Rounded")? {
                    return Err(new_decimal_error(vm, "Rounded", "rounded result"));
                }
                return Ok(Dec {
                    sign: false,
                    coeff: BigInt::from(10).pow(pad),
                    exp: -(pad as isize),
                    special: Special::Finite,
                });
            } else {
                return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
            }
        }
        // b = -Inf: Inf**(-Inf) = 0 (powx580); -Inf**(-Inf) = NaN (powx600).
        if a.special == Special::Infinity && b.special == Special::Infinity && b.sign {
            if a.sign {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "-Inf ** -Inf"));
                }
                return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
            }
            return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Finite });
        }
        // b = -Inf with a finite base: the inverse of b = +Inf
        // (|a| > 1 -> 0; |a| < 1 -> Inf; |a| = 1 -> 1; negative -> NaN).
        // 0 ** -Inf = +Infinity (powx614: -0 ** -Inf -> Infinity).
        if b.sign && a.special == Special::Finite && a.is_zero() {
            return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
        }
        if b.sign && a.special == Special::Finite && !a.is_zero() {
            if a.sign {
                ctx.set_flag(vm, "InvalidOperation")?;
                if ctx.trap(vm, "InvalidOperation")? {
                    return Err(new_decimal_error(vm, "InvalidOperation", "negative ** -Inf"));
                }
                return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
            }
            let adj = a.exp + a.coeff_digits() as isize - 1;
            if adj > 0 {
                return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Finite });
            } else if adj < 0 {
                return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
            } else if a.coeff == BigInt::from(1) {
                let pad = (ctx.prec - 1).max(0) as u32;
                ctx.set_flag(vm, "Inexact")?;
                if ctx.trap(vm, "Inexact")? {
                    return Err(new_decimal_error(vm, "Inexact", "inexact result"));
                }
                ctx.set_flag(vm, "Rounded")?;
                if ctx.trap(vm, "Rounded")? {
                    return Err(new_decimal_error(vm, "Rounded", "rounded result"));
                }
                return Ok(Dec {
                    sign: false,
                    coeff: BigInt::from(10).pow(pad),
                    exp: -(pad as isize),
                    special: Special::Finite,
                });
            } else {
                return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Finite });
            }
        }
        // b = NaN (not Inf): propagate the NaN with its payload (powx676:
        // power Inf NaN9 -> NaN9, not generic NaN).
        if b.special == Special::NaN {
            return Ok(b.clone());
        }
        return Ok(Dec { sign: a.sign, coeff: BigInt::from(0), exp: 0, special: b.special });
    }
    let is_integer = !(b.exp < 0
        && (&b.coeff % BigInt::from(10).pow((-b.exp) as u32)) != BigInt::from(0));
    if a.special != Special::Finite {
        // The base being special is handled before any arithmetic, or an
        // infinite base falls through to the integer path on coefficient 0
        // and comes back as 0 (Infinity ** 2 must be Infinity).
        if a.special == Special::SignalingNaN {
            ctx.set_flag(vm, "InvalidOperation")?;
            if ctx.trap(vm, "InvalidOperation")? {
                return Err(new_decimal_error(vm, "InvalidOperation", "sNaN operand"));
            }
            // Quiet the sNaN with its payload (powx673: power sNaN8 1 -> NaN8).
            let mut n = a.clone();
            n.special = Special::NaN;
            return Ok(n);
        }
        if a.special == Special::NaN {
            return Ok(a.clone());
        }
        if !is_integer {
            // A fractional exponent has no parity case: let the exp(b*ln(a))
            // path below handle infinite bases.
        } else {
            let n: i64 = if b.exp < 0 {
                let pow = BigInt::from(10).pow((-b.exp) as u32);
                (&b.coeff / &pow).to_string().parse().unwrap_or(0)
            } else {
                (&b.coeff * BigInt::from(10).pow(b.exp as u32)).to_string().parse().unwrap_or(0)
            };
            let n = if b.sign { -n } else { n };
            return Ok(match n {
                0 => Dec { sign: false, coeff: BigInt::from(1), exp: 0, special: Special::Finite },
                // 1 / Infinity**m is zero; a negative base keeps -Infinity for
                // odd m only.
                n if n < 0 => {
                    // 1 / Inf^|n| = 0; sign negative for -Inf with odd |n|
                    // (powx602: power -Inf -1 -> -0).
                    let neg = a.sign && (n.unsigned_abs() % 2 == 1);
                    Dec { sign: neg, coeff: BigInt::from(0), exp: 0, special: Special::Finite }
                }
                n if n % 2 == 0 => Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Infinity },
                _ => Dec { sign: a.sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity },
            });
        }
    }
    if !is_integer && a.sign && !a.is_zero() {
        // A negative base with a fractional exponent is undefined:
        // (-1) ** 0.5 raises InvalidOperation rather than returning a value.
        ctx.set_flag(vm, "InvalidOperation")?;
        if ctx.trap(vm, "InvalidOperation")? {
            return Err(new_decimal_error(vm, "InvalidOperation", "negative base with fractional exponent"));
        }
        return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
    }
    if b.exp > 0 {
        // positive exponent, still integer
    } else if b.exp < 0 && !is_integer {
        // A zero base is 0 for any positive fractional exponent. Going through
        // exp(b * ln(a)) does not give that: ln(0) is -Infinity, whose
        // coefficient is zero, so is_zero() matches it and the sign is lost on
        // the way back (0 ** 0.5 came out as Infinity).
        if a.is_zero() && !b.sign {
            return Ok(Dec::zero());
        }
        // Fractional exponent: compute via exp(n * ln(a)) at high precision.
        // We build a wide (extra-precision) context, apply rounding at the
        // end with the caller's context (which may overflow -> Infinity).
        let wide_prec = (ctx.prec as usize + 20).max(30);
        let wide = Ctx::from_parts(
            wide_prec as isize,
            Rounding::HalfEven,
            ctx.emax,
            ctx.emin,
            ctx.clamp,
            vm.ctx.new_dict(),
            vm.ctx.new_dict(),
        );
        // a**b == exp(b * ln(a)), evaluated at extra precision and rounded
        // once at the end with the caller's context (which may overflow).
        let r = dec_exp(vm, &wide, &dec_mul(&wide, vm, b, &dec_ln(vm, &wide, a)?)?)?;
        // If the result overflowed in the wide context, the flags were set
        // in the wide ctx's dict (lost). Re-signal in the caller's ctx.
        // Only for FINITE bases: an Infinity base yields Infinity from the
        // input (powx586: Inf ** 0.5 -> Infinity, no overflow).
        if r.special == Special::Infinity && a.special == Special::Finite {
            ctx.set_flag(vm, "Overflow")?;
            if ctx.trap(vm, "Overflow")? { return Err(ctx_trap_exc(vm, "Overflow", "overflow")); }
            ctx.set_flag(vm, "Inexact")?; if ctx.trap(vm, "Inexact")? { return Err(new_decimal_error(vm, "Inexact", "inexact result")); }
            ctx.set_flag(vm, "Rounded")?; if ctx.trap(vm, "Rounded")? { return Err(new_decimal_error(vm, "Rounded", "rounded result")); }
            return Ok(r);
        }
        let mut r = apply_context(vm, ctx, r)?;
        // CPython's non-integer power keeps the full context width even when the
        // result is exact: 1 ** 0.5 is 1.000000000000000000000000000. This is the
        // opposite of the integral-quotient rule log10 and divmod follow.
        // The non-integer power is transcendental: always inexact (powx2015:
        // power 1 12.3 -> 1.00 Inexact Rounded). But zero/special results from
        // extreme inputs (Inf ** -0.5 -> 0) are exact, not inexact (powx583).
        if !r.is_zero() && !r.is_special() {
            // Only pad to full precision if the result is NORMAL (adjusted >=
            // Emin). Subnormal results keep their natural coefficient width
            // (pwsx804: power 1E-19 0.5 at prec 11/Emin -9 -> 3.162277660E-10,
            // 10 digits, not 3.1622776600E-10 with a trailing zero).
            let adj = r.exp + coeff_digits_of(&r.coeff) as isize - 1;
            if adj >= ctx.emin {
                let digits = coeff_digits_of(&r.coeff);
                if digits < ctx.prec as usize {
                    let pad = ctx.prec as usize - digits;
                    r.coeff *= BigInt::from(10).pow(pad as u32);
                    r.exp -= pad as isize;
                    // The padding means the computation's guard digits didn't
                    // fill the full precision -> inexact (powx2015: power 1
                    // 12.3 at prec 2 -> 1.00 Inexact Rounded). But if no
                    // padding was needed the result is exact (powx4007: power
                    // 1 1.1E+999999 at prec 1 -> 1, no flags).
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
        }
        return Ok(r);
    }
    // Parse the integer exponent. If b.exp > 18 the value 10^b.exp doesn't
    // fit in i64 (powx519: 100 ** 1E+19); if b.exp < -18 and b.coeff has
    // fewer digits than |b.exp| the quotient is 0. Either way avoid building
    // 10^|b.exp| which could be a billion-digit BigInt (1E+999999999).
    let mut n: i64;
    if b.exp > 18 && b.coeff != BigInt::from(0) {
        // Huge positive exponent: doesn't fit i64 (avoid 10^b.exp).
        n = i64::MAX;  // sentinel — the huge-exponent handler below catches this
    } else if b.exp > 18 && b.coeff == BigInt::from(0) {
        n = 0;  // 0E+k is just zero, not a huge number
    } else if b.exp < -18 && (coeff_digits_of(&b.coeff) as isize) <= (-b.exp) {
        n = 0;  // coeff / 10^|exp| = 0
    } else {
        let n_str = if b.exp < 0 {
            let pow = BigInt::from(10).pow((-b.exp) as u32);
            let q = &b.coeff / &pow;
            q.to_string()
        } else {
            (&b.coeff * BigInt::from(10).pow(b.exp as u32)).to_string()
        };
        n = match n_str.parse() {
            Ok(v) => v,
            Err(_) => {
                // Exponent too large for i64 (powx519: 100 ** 1E+19). The
                // magnitude is astronomically large, so the result depends on
                // |a| vs 1 and the sign of b, like ±Inf.
                let a_adj = a.exp + a.coeff_digits() as isize - 1;
                let pos = !b.sign;
                let gt_one = a_adj > 0 || (a_adj == 0 && a.coeff > BigInt::from(1));
                let lt_one = a_adj < 0 || (a_adj == 0 && a.coeff < BigInt::from(1) && !a.is_zero());
                if a.is_zero() {
                    if pos { return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Finite }); }
                    return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
                }
                if a.sign {
                    ctx.set_flag(vm, "InvalidOperation")?;
                    if ctx.trap(vm, "InvalidOperation")? { return Err(new_decimal_error(vm, "InvalidOperation", "huge exponent, parity unknown")); }
                    return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
                }
                if gt_one {
                    if pos {
                        ctx.set_flag(vm, "Overflow")?;
                        if ctx.trap(vm, "Overflow")? { return Err(ctx_trap_exc(vm, "Overflow", "overflow")); }
                        ctx.set_flag(vm, "Inexact")?; if ctx.trap(vm, "Inexact")? { return Err(new_decimal_error(vm, "Inexact", "inexact result")); }
                        ctx.set_flag(vm, "Rounded")?; if ctx.trap(vm, "Rounded")? { return Err(new_decimal_error(vm, "Rounded", "rounded result")); }
                        return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
                    }
                    return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Finite });
                } else if lt_one {
                    if pos { return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Finite }); }
                    ctx.set_flag(vm, "Overflow")?;
                    if ctx.trap(vm, "Overflow")? { return Err(ctx_trap_exc(vm, "Overflow", "overflow")); }
                    return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
                } else {
                    // |a| = 1: 1^n = 1 (exact, no flags — powx4007: power 1
                    // 1.1E+999999 -> 1, not 1.000...0 with Inexact+Rounded).
                    return Ok(Dec { sign: false, coeff: BigInt::from(1), exp: 0, special: Special::Finite });
                }
            }
        };
    }
    // The i64::MAX sentinel from the b.exp > 18 path: handle as huge.
    // Also catches -i64::MAX (negative huge exponent) which would otherwise
    // overflow the squaring loop's exp *= 2.
    if n.abs() == i64::MAX && b.exp > 18 {
        let a_adj = a.exp + a.coeff_digits() as isize - 1;
        let pos = !b.sign;
        let gt_one = a_adj > 0 || (a_adj == 0 && a.coeff > BigInt::from(1));
        let lt_one = a_adj < 0 || (a_adj == 0 && a.coeff < BigInt::from(1) && !a.is_zero());
        if a.is_zero() {
            if pos { return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Finite }); }
            return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
        }
        if a.sign {
            ctx.set_flag(vm, "InvalidOperation")?;
            if ctx.trap(vm, "InvalidOperation")? { return Err(new_decimal_error(vm, "InvalidOperation", "huge exponent, parity unknown")); }
            return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
        }
        if gt_one {
            if pos {
                ctx.set_flag(vm, "Overflow")?;
                if ctx.trap(vm, "Overflow")? { return Err(ctx_trap_exc(vm, "Overflow", "overflow")); }
                ctx.set_flag(vm, "Inexact")?; if ctx.trap(vm, "Inexact")? { return Err(new_decimal_error(vm, "Inexact", "inexact result")); }
                ctx.set_flag(vm, "Rounded")?; if ctx.trap(vm, "Rounded")? { return Err(new_decimal_error(vm, "Rounded", "rounded result")); }
                return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
            }
            return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Finite });
        } else if lt_one {
            if pos { return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Finite }); }
            ctx.set_flag(vm, "Overflow")?;
            if ctx.trap(vm, "Overflow")? { return Err(ctx_trap_exc(vm, "Overflow", "overflow")); }
            return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
        } else {
            // |a| = 1: 1^n = 1 (exact, no flags — powx4007).
            return Ok(Dec { sign: false, coeff: BigInt::from(1), exp: 0, special: Special::Finite });
        }
    }
    if b.sign {
        n = -n;
    }
    if n < 0 {
        // 0 ** (-n) is +Infinity without Division_by_zero (decTest powx126:
        // power 0 -1 -> Infinity; CPython's mpd_qpow does not signal).
        // -0 ** (-odd) is -Infinity (powx128: power -0 -1 -> -Infinity).
        if a.is_zero() {
            let neg = a.sign && (n.unsigned_abs() % 2 == 1);
            return Ok(Dec { sign: neg, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
        }
        // Negative exponent: a^-m = 1 / a^m, computed from the exact power
        // (never the context-rounded base: dec_pow_int at prec 5 rounds
        // 12345^3 to 1.8814E+12, losing the precision needed for 1/a^3 ->
        // 5.3153E-13).
        let m = n.unsigned_abs() as i64;
        let large_neg = m > ctx.prec as i64 * 2;
        let max_digits = (ctx.prec as usize + 10).max(20);
        let mut rp = Dec { sign: false, coeff: BigInt::from(1), exp: 0, special: Special::Finite };
        let mut coeff = a.coeff.clone();
        let mut exp = a.exp;
        let mut be = m;
        let ten = BigInt::from(10);
        while be > 0 {
            if be & 1 == 1 {
                rp.coeff *= &coeff;
                rp.exp += exp;
                if large_neg {
                    let cd = coeff_digits_of(&rp.coeff);
                    if cd > max_digits {
                        let drop = cd - max_digits;
                        let p = ten.pow(drop as u32);
                        rp.coeff /= &p;
                        rp.exp += drop as isize;
                    }
                }
            }
            be >>= 1;
            if be > 0 {
                coeff = &coeff * &coeff;
                exp *= 2;
                if large_neg {
                    let cd = coeff_digits_of(&coeff);
                    if cd > max_digits {
                        let drop = cd - max_digits;
                        let p = ten.pow(drop as u32);
                        coeff /= &p;
                        exp += drop as isize;
                    }
                }
            }
        }
        // The sign of a^m: negative for odd m with a negative base
        // (powx448: power -0.04E+334 -3 -> -1.5625E-998, not positive).
        rp.sign = a.sign && (m % 2 == 1);
        let one = Dec { sign: false, coeff: BigInt::from(1), exp: 0, special: Special::Finite };
        return dec_div(ctx, vm, &one, &rp);
    }
    // 0 ** n (n > 0) is zero with exponent 0, not n*exp (decTest powx223:
    // power 0E-30 3 -> 0, not 0E-90). The sign is negative for odd n and a
    // negative base.
    if a.is_zero() {
        return Ok(Dec {
            sign: a.sign && (n % 2 == 1),
            coeff: BigInt::from(0),
            exp: 0,
            special: Special::Finite,
        });
    }
    // Before the expensive repeated-squaring, check whether the result would
    // be far out of range. For coeff=1 the result is exactly 10^(n*exp). For
    // coeff>1 the lower bound of the adjusted is n*(exp+1) (coeff^n >= 10 for
    // large n) and the upper bound is n*(exp+digits)-1. If the lower bound is
    // above Emax -> Overflow; if the upper bound is below Etiny -> flush to 0
    // (powx397: 0.02 ** 100000000 would otherwise build a 67M-digit BigInt).
    let etiny = ctx.emin - (ctx.prec as isize - 1);
    if n != 0 && n.abs() > ctx.prec as i64 * 2 {
        let (lower, upper): (i64, i64) = if a.coeff == BigInt::from(1) {
            let adj = n * a.exp as i64;
            (adj, adj)
        } else {
            // Lower bound: coeff^n has at least 1 digit per power ->
            // adjusted >= n*(exp+digits-1). Upper bound: coeff^n has at most
            // n*digits digits -> adjusted <= n*(exp+digits)-1.
            (n * (a.exp + a.coeff_digits() as isize - 1) as i64,
             n * (a.exp + a.coeff_digits() as isize) as i64 - 1)
        };
        if upper < etiny as i64 {
            ctx.set_flag(vm, "Subnormal")?;
            if ctx.trap(vm, "Subnormal")? { return Err(ctx_trap_exc(vm, "Subnormal", "subnormal")); }
            ctx.set_flag(vm, "Underflow")?;
            if ctx.trap(vm, "Underflow")? { return Err(ctx_trap_exc(vm, "Underflow", "underflow")); }
            ctx.set_flag(vm, "Inexact")?;
            if ctx.trap(vm, "Inexact")? { return Err(new_decimal_error(vm, "Inexact", "inexact result")); }
            ctx.set_flag(vm, "Rounded")?;
            if ctx.trap(vm, "Rounded")? { return Err(new_decimal_error(vm, "Rounded", "rounded result")); }
            ctx.set_flag(vm, "Clamped")?;
            if ctx.trap(vm, "Clamped")? { return Err(ctx_trap_exc(vm, "Clamped", "clamped")); }
            return Ok(Dec { sign: a.sign && (n % 2 == 1), coeff: BigInt::from(0), exp: etiny, special: Special::Finite });
        }
        if lower > ctx.emax as i64 {
            ctx.set_flag(vm, "Overflow")?;
            if ctx.trap(vm, "Overflow")? { return Err(ctx_trap_exc(vm, "Overflow", "overflow")); }
            ctx.set_flag(vm, "Inexact")?;
            if ctx.trap(vm, "Inexact")? { return Err(new_decimal_error(vm, "Inexact", "inexact result")); }
            ctx.set_flag(vm, "Rounded")?;
            if ctx.trap(vm, "Rounded")? { return Err(new_decimal_error(vm, "Rounded", "rounded result")); }
            return Ok(Dec { sign: a.sign && (n % 2 == 1), coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
        }
    }
    let large_n = n.abs() > ctx.prec as i64 * 2;
    let max_digits = (ctx.prec as usize + 10).max(20);
    let mut result = Dec { sign: false, coeff: BigInt::from(1), exp: 0, special: Special::Finite };
    // Integer power via repeated squaring. For large exponents (n > prec*2)
    // the full coeff^n would be a multi-million-digit BigInt (powx183:
    // 7**1000000). CPython's mpd_qpow keeps limited precision throughout;
    // we drop low digits (keeping prec+10 guard digits) at each step so the
    // multiplications stay O((prec+10)^2) and the final rounding is correct.
    let mut coeff = a.coeff.clone();
    let mut exp = a.exp;
    let mut base_exp = n;
    let ten = BigInt::from(10);
    while base_exp > 0 {
        if base_exp & 1 == 1 {
            result.coeff *= &coeff;
            result.exp += exp;
            if large_n {
                let cd = coeff_digits_of(&result.coeff);
                if cd > max_digits {
                    let drop = cd - max_digits;
                    let p = ten.pow(drop as u32);
                    result.coeff /= &p;
                    result.exp += drop as isize;
                }
            }
        }
        base_exp >>= 1;
        if base_exp > 0 {
            coeff = &coeff * &coeff;
            exp *= 2;
            if large_n {
                let cd = coeff_digits_of(&coeff);
                if cd > max_digits {
                    let drop = cd - max_digits;
                    let p = ten.pow(drop as u32);
                    coeff /= &p;
                    exp += drop as isize;
                }
            }
        }
    }
    result.sign = a.sign && (n % 2 == 1);
    apply_context(vm, ctx, result)
}

/// Compute a ** b % m (modular exponentiation) for integer operands.
/// Requires a, b as non-negative integers (exp >= 0); used by
/// Decimal.__pow__/Context.power with a modulo argument.
pub(crate) fn dec_pow_mod(ctx: &Ctx, vm: &VirtualMachine, a: &Dec, b: &Dec, m: &Dec) -> PyResult<Dec> {
    // Require integer operands with exp >= 0 (value = coeff * 10^exp).
    let intval = |d: &Dec| -> Option<BigInt> {
        if d.special != Special::Finite || d.exp < 0 {
            return None;
        }
        Some(d.coeff.clone() * BigInt::from(10).pow(d.exp as u32))
    };
    let (Some(av), Some(bv), Some(mv)) = (intval(a), intval(b), intval(m)) else {
        return Err(new_decimal_error(vm, "InvalidOperation", "modulo power requires integers"));
    };
    if bv.sign() != Sign::Plus {
        return Err(new_decimal_error(vm, "InvalidOperation", "modulo power exponent must be non-negative"));
    }
    if mv == BigInt::from(0) {
        return Err(new_decimal_error(vm, "DivisionByZero", "modulo zero"));
    }
    // result = av^bv mod mv via modular exponentiation
    let mut base = if av.sign() == Sign::Minus { -av } else { av.clone() };
    base %= &mv;
    let mut result = BigInt::from(1);
    let mut exp = bv.clone();
    while exp > BigInt::from(0) {
        if (&exp % BigInt::from(2)) == BigInt::from(1) {
            result = (&result * &base) % &mv;
        }
        exp >>= 1u32;
        if exp > BigInt::from(0) {
            base = (&base * &base) % &mv;
        }
    }
    // result sign: a^b % m follows the sign of m in the common case; CPython
    // returns a non-negative result when m is positive.
    // CPython: the modulo-power result must fit in the context precision;
    // otherwise InvalidOperation is raised (e.g. pow(1000,1,501) with
    // prec=2 -> 499 has 3 digits).
    if coeff_digits_of(&result) > ctx.prec as usize {
        ctx.set_flag(vm, "InvalidOperation")?;
        if ctx.trap(vm, "InvalidOperation")? {
            return Err(new_decimal_error(vm, "InvalidOperation", "result too many digits in modulo power"));
        }
        return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
    }
    Ok(Dec { sign: false, coeff: result, exp: 0, special: Special::Finite })
}

/// floor division a // b and modulo a % b, computed exactly (sign of divisor
/// for modulo, floor for division). Returns (floor, mod), or `None` when the
/// integer quotient needs more than `prec` digits (CPython's
/// DivisionImpossible).
///
/// The size guards mirror `_pydecimal._divide` and run before any 10^N
/// scaling, so an extreme exponent difference can never materialise a giant
/// intermediate integer: divideint.decTest dvix274 (9e999999999 // 1) must
/// report Division_impossible instead of building 10^999999999.
pub(crate) fn floor_div_mod(a: &Dec, b: &Dec, prec: isize) -> Option<(Dec, Dec)> {
    // Specials per CPython:
    //   divmod(inf, finite)   = (Inf, NaN)
    //   divmod(finite, inf)   = (0, a)
    //   divmod(inf, inf)      = (NaN, NaN)
    //   divmod(nan, x)        = (NaN, NaN)
    //   divmod(x, 0)          = (NaN, NaN)
    let nan = Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN };
    let is_nan = |d: &Dec| d.special == Special::NaN || d.special == Special::SignalingNaN;
    if is_nan(a) || is_nan(b) {
        return Some((nan.clone(), nan.clone()));
    }
    let zero = Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Finite };
    if b.is_zero() {
        // b == 0:
        //   a == 0 -> (NaN, NaN) [InvalidOperation]
        //   a != 0 -> (Infinity, NaN) [InvalidOperation + DivisionByZero]
        if a.is_zero() {
            return Some((nan.clone(), nan.clone()));
        }
        return Some((
            Dec { sign: a.sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity },
            nan.clone(),
        ));
    }
    if a.is_special() {
        // a == Infinity
        if b.is_special() {
            // inf / inf -> (NaN, NaN)
            return Some((nan.clone(), nan.clone()));
        }
        // inf / finite -> (Inf, NaN)
        return Some((
            Dec { sign: a.sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity },
            nan.clone(),
        ));
    }
    if b.is_special() {
        // finite / inf -> (0, a)
        return Some((zero, a.clone()));
    }
    // CPython `_divide` size bounds, decided before any scaling. The digit
    // count of the quotient is bounded by the adjusted-exponent difference,
    // so both "the quotient is zero" and "the quotient is too large" are
    // settled without building 10^|a.exp - b.exp|, which for an operand like
    // 1e999999999 would be a ~10^9-digit integer.
    let expdiff = (a.exp + coeff_digits_of(&a.coeff) as isize)
        - (b.exp + coeff_digits_of(&b.coeff) as isize);
    if a.is_zero() || expdiff <= -2 {
        // |a| < |b| / 10 (or a == 0): quotient 0, remainder is the dividend
        // rescaled to min(a.exp, b.exp). The rescale factor stays below b's
        // digit count because expdiff <= -2 bounds a.exp - b.exp by it.
        let ideal_exp = a.exp.min(b.exp);
        let m = if a.is_zero() || a.exp == ideal_exp {
            Dec { sign: a.sign, coeff: a.coeff.clone(), exp: ideal_exp, special: Special::Finite }
        } else {
            Dec {
                sign: a.sign,
                coeff: &a.coeff * BigInt::from(10).pow((a.exp - ideal_exp) as u32),
                exp: ideal_exp,
                special: Special::Finite,
            }
        };
        return Some((
            Dec { sign: a.sign ^ b.sign, coeff: BigInt::from(0), exp: 0, special: Special::Finite },
            m,
        ));
    }
    if expdiff > prec {
        // The quotient needs at least prec+1 digits -> DivisionImpossible.
        return None;
    }
    // value = ca*10^ea / (cb*10^eb)
    let e = a.exp - b.exp;
    let (num, den) = if e >= 0 {
        (&a.coeff * BigInt::from(10).pow(e as u32), b.coeff.clone())
    } else {
        (a.coeff.clone(), &b.coeff * BigInt::from(10).pow((-e) as u32))
    };
    let q = &num / &den;
    if coeff_digits_of(&q) > prec as usize {
        // CPython `_divide`: q < 10**prec, otherwise DivisionImpossible.
        return None;
    }
    let neg = a.sign ^ b.sign;
    // CPython Decimal uses TRUNCATED division for // and % (sign of dividend
    // for %), unlike int which uses floor. -10//4 = -2, -10%4 = -2.
    let floor = Dec { sign: neg, coeff: q, exp: 0, special: Special::Finite };
    // mod = a - b*floor, exact
    let bf_coeff = &b.coeff * &floor.coeff;
    let bf_exp = b.exp + floor.exp;
    let me = a.exp.min(bf_exp);
    let ma = &a.coeff * BigInt::from(10).pow((a.exp - me) as u32);
    let mb = &bf_coeff * BigInt::from(10).pow((bf_exp - me) as u32);
    let va = if a.sign { -ma } else { ma };
    let prod_sign = b.sign ^ floor.sign;
    let vb = if prod_sign { -mb } else { mb };
    let rv = va - vb;
    // CPython: a zero remainder takes the sign of the dividend
    // (remainder(-0.0, -1) -> -0.0; remainder(-0.00, 1) -> -0.00).
    let msign = if rv == BigInt::from(0) { a.sign } else { rv.sign() == Sign::Minus };
    let mcoeff = rv.abs();
    let m = Dec { sign: msign, coeff: mcoeff, exp: me, special: Special::Finite };
    Some((floor, m))
}

/// floor division a // b (context applied)
pub(crate) fn dec_floordiv(ctx: &Ctx, vm: &VirtualMachine, a: &Dec, b: &Dec) -> PyResult<Dec> {
    // Specials first: NaN/Infinity operands win over division-by-zero.
    if a.is_special() || b.is_special() {
        return special_binary(vm, ctx, a, b, "floordiv");
    }
    if b.is_zero() {
        if a.is_zero() {
            // 0 // 0 -> Division_undefined (InvalidOperation) + NaN
            // (dvix202: divideint(0.0E5, 0) -> NaN Division_undefined).
            ctx.set_flag(vm, "InvalidOperation")?;
            if ctx.trap(vm, "InvalidOperation")? {
                return Err(new_decimal_error(vm, "InvalidOperation", "division undefined"));
            }
            return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
        }
        ctx.set_flag(vm, "DivisionByZero")?;
        if ctx.trap(vm, "DivisionByZero")? {
            return Err(new_decimal_error(vm, "DivisionByZero", "division by zero"));
        }
        // x // 0 -> Infinity; the sign follows the quotient's sign
        // (a.sign XOR b.sign): divideint(1,-0.0) -> -Infinity,
        // divideint(-1,-0.0) -> Infinity (CPython).
        return Ok(Dec {
            sign: a.sign ^ b.sign,
            coeff: BigInt::from(0),
            exp: 0,
            special: Special::Infinity,
        });
    }
    match floor_div_mod(a, b, ctx.prec) {
        Some((floor, _)) => apply_context(vm, ctx, floor),
        None => {
            // CPython: the quotient needs more than prec digits ->
            // DivisionImpossible (dvix274: divideint(9e999999999, 1)).
            ctx.set_flag(vm, "InvalidOperation")?;
            if ctx.trap(vm, "InvalidOperation")? {
                return Err(new_decimal_error(vm, "InvalidOperation", "division impossible"));
            }
            Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN })
        }
    }
}

/// modulo a % b (sign of divisor)
pub(crate) fn dec_mod(ctx: &Ctx, vm: &VirtualMachine, a: &Dec, b: &Dec) -> PyResult<Dec> {
    // Specials first: a NaN operand wins over division-by-zero checks
    // (remainder(NaN, -0) -> NaN).
    if a.is_special() || b.is_special() {
        return special_binary(vm, ctx, a, b, "mod");
    }
    if b.is_zero() {
        ctx.set_flag(vm, "InvalidOperation")?;
        if ctx.trap(vm, "InvalidOperation")? {
            return Err(new_decimal_error(vm, "InvalidOperation", "division by zero"));
        }
        return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
    }
    match floor_div_mod(a, b, ctx.prec) {
        Some((_, m)) => apply_context(vm, ctx, m),
        None => {
            // CPython: if the integer quotient needs more than prec digits,
            // the remainder cannot be computed exactly -> DivisionImpossible
            // (remainder(100000000.5, 1) at prec 8; remainder(9.9e999999999, 1)).
            ctx.set_flag(vm, "InvalidOperation")?;
            if ctx.trap(vm, "InvalidOperation")? {
                return Err(new_decimal_error(vm, "InvalidOperation", "division impossible"));
            }
            Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN })
        }
    }
}

/// Format a finite decimal for `format()` / `__format__`.
/// Supports the CPython Decimal format spec: `[[fill]align][sign][#][0][width][,][.precision][type]`
/// with types e/E/f/F/g/G/n/%. Rounding is round-half-even via context.
/// Locale formatting data used by the 'n' format and ','/_ grouping.
/// Mirrors `localeconv()` / decimal.__format__ localeconv argument.
#[derive(Clone, Default)]
pub(crate) struct Locale {
    /// Decimal point separator ("" uses '.').
    pub decimal_point: String,
    /// Thousands separator ("" collapses to no grouping).
    pub thousands_sep: String,
    /// Grouping byte code: e.g. [3, 3, 0], [1, 4, 2, <CHAR_MAX=127>].
    pub grouping: Vec<i32>,
}

pub(crate) fn format_decimal(
    d: &Dec,
    spec: &str,
    vm: &VirtualMachine,
    ctx: &Ctx,
    capitals: isize,
    locale: &Locale,
) -> Result<String, String> {
    // Parse the format spec.
    let mut spec = spec.to_string();
    // Type char: valid types are e/E/f/F/g/G/n/%. A trailing alphabetic
    // char that is not a valid type is an error (e.g. 'fz').
    let type_char = if spec.ends_with('%') {
        spec.pop();
        Some('%')
    } else if spec.is_empty() {
        None
    } else {
        let last = spec.chars().last().unwrap();
        match last {
            'e' | 'E' | 'f' | 'F' | 'g' | 'G' | 'n' | 'N' => {
                spec.pop();
                Some(last)
            }
            _ => None,
        }
    };
    // Precision: `.N` or `.N_`/`.N,` (fractional separator) or `._`/`,`
    // (fraction separator without digits). The integer thousands separator
    // may be `,` or `_` (Python 3.13+).
    let mut precision: Option<usize> = None;
    let mut frac_sep: Option<char> = None;
    if let Some(dot) = spec.rfind('.') {
        let after = &spec[dot + 1..];
        // digits possibly followed by a fractional separator char
        let digits_end = after
            .char_indices()
            .find(|(_, c)| !c.is_ascii_digit())
            .map(|(i, _)| i)
            .unwrap_or(after.len());
        let digits = &after[..digits_end];
        let trailing = after[digits_end..].chars().next();
        let rest_after_trailing = if trailing.is_some() {
            after[digits_end..].chars().nth(1).is_some()
        } else {
            false
        };
        if !digits.is_empty() {
            precision = Some(digits.parse().unwrap_or(0));
            spec.truncate(dot);
            // optional fractional separator after precision
            if let Some(c @ (',' | '_')) = trailing {
                frac_sep = Some(c);
                // nothing may follow the fractional separator (e.g. '.6_X')
                if rest_after_trailing {
                    return Err(format!("invalid format string: '{c}'"));
                }
            } else if trailing.is_some() {
                // digits then an invalid trailing char (e.g. '.6fz')
                return Err(format!("invalid format string: '{}'", trailing.unwrap()));
            }
        } else if let Some(c @ (',' | '_')) = trailing {
            // no precision, but a fractional separator: strip the dot
            frac_sep = Some(c);
            if rest_after_trailing {
                return Err(format!("invalid format string: '{c}'"));
            }
            spec.truncate(dot);
        }
    }
    // Remaining spec is fill/align/sign/width/grouping. Validate the
    // characters (raise ValueError on anything unknown like 'fz'); full
    // alignment padding is not implemented (suite focuses on numeric part).
    let rest = spec;
    // Parse fill/align/width/grouping from the remaining spec.
    let mut align = '>';
    let mut fill = ' ';
    let mut width: usize = 0;
    let mut grouping: Option<char> = None;
    let mut sign_flag = '-';
    let mut coerce_negzero = false;
    let mut alternate = false;
    if !rest.is_empty() {
        // fill/align: optional fill char + one of <>=^
        let rbytes: Vec<char> = rest.chars().collect();
        let mut idx = 0;
        if idx + 1 < rbytes.len() && ['<', '>', '=', '^'].contains(&rbytes[idx + 1]) {
            fill = rbytes[idx];
            align = rbytes[idx + 1];
            idx += 2;
        } else if idx < rbytes.len() && ['<', '>', '=', '^'].contains(&rbytes[idx]) {
            align = rbytes[idx];
            idx += 1;
        }
        // sign flag (+ - space); 'z' is a separate option that coerces a
        // negative zero to an unsigned zero before the sign is applied.
        // e.g. 'z'  -> -0 => '0'; '+z' -> -0 => '+0'; ' z' -> -0 => ' 0'.
        // NB: 'z' as a FILL char (z< / z> / z= / z^) is handled above and
        // must NOT be treated as the coerce option.
        if idx < rbytes.len() && (rbytes[idx] == '+' || rbytes[idx] == '-' || rbytes[idx] == ' ') {
            sign_flag = rbytes[idx];
            idx += 1;
            // 'z' may follow a sign flag: '+z', ' z', '-z'.
            if idx < rbytes.len() && rbytes[idx] == 'z' {
                coerce_negzero = true;
                idx += 1;
            }
        } else if idx < rbytes.len() && rbytes[idx] == 'z'
            && (idx + 1 >= rbytes.len() || !['<', '>', '=', '^'].contains(&rbytes[idx + 1]))
        {
            // 'z' as the sign group itself (a following non-align char, or end).
            coerce_negzero = true;
            idx += 1;
        }
        // '0' means zero-pad after sign
        if idx < rbytes.len() && rbytes[idx] == '0' {
            fill = '0';
            idx += 1;
        }
        if idx < rbytes.len() && rbytes[idx] == '#' {
            alternate = true;
            idx += 1;
        }
        // width (digits)
        let mut wdigits = String::new();
        while idx < rbytes.len() && rbytes[idx].is_ascii_digit() {
            wdigits.push(rbytes[idx]);
            idx += 1;
        }
        if !wdigits.is_empty() {
            // CPython raises ValueError when the requested width exceeds
            // internal limits (avoids a pathological allocation).
            let parsed: u128 = wdigits.parse().unwrap_or(u128::MAX);
            if parsed > 2_000_000_000 {
                return Err("format specification exceeds internal limits of _decimal".to_string());
            }
            width = parsed as usize;
        }
        // grouping (after width, per format grammar)
        if idx < rbytes.len() && (rbytes[idx] == ',' || rbytes[idx] == '_') {
            grouping = Some(rbytes[idx]);
            idx += 1;
        }
        // an optional second grouping char (e.g. `,._e` splits precision
        // separator; ignore the trailing `_`/`,` here)
        if idx < rbytes.len() && (rbytes[idx] == ',' || rbytes[idx] == '_') {
            idx += 1;
        }
        // validate remaining
        for c in rbytes[idx..].iter() {
            if !c.is_ascii_digit() {
                return Err(format!("invalid format string: '{c}'"));
            }
        }
    }
    // No type char defaults to 'g' or 'G' depending on context.capitals.
    let ty = match type_char {
        Some(c) => c,
        None => if capitals == 1 { 'G' } else { 'g' },
    };
    // Sign prefix from spec's sign flag + value sign.
    // `+`: always show sign; `-`: only negatives; ` `: space for positive.
    let force_sign = sign_flag == '+';
    let space_sign = sign_flag == ' ';
    // Also strip the sign flag from `rest` so it isn't misread below.
    let mut neg = d.sign;
    // 'z' option: a negative zero formats as unsigned zero.
    if coerce_negzero && d.is_zero() {
        neg = false;
    }
    if d.is_special() {
        let payload = if d.coeff == BigInt::from(0) {
            String::new()
        } else {
            d.coeff_str()
        };
        let mut s = match d.special {
            Special::Infinity => "Infinity".to_string(),
            Special::NaN => format!("NaN{payload}"),
            Special::SignalingNaN => format!("sNaN{payload}"),
            Special::Finite => unreachable!(),
        };
        if neg {
            s.insert(0, '-');
        } else if force_sign {
            s.insert(0, '+');
        } else if space_sign {
            s.insert(0, ' ');
        }
        // '%' type appends a percent sign for specials too.
        let mut padded = s.clone();
        if ty == '%' {
            padded.push('%');
        }
        // Apply width padding (NaN/Inf subject to fill/align).
        if padded.chars().count() < width {
            padded = apply_padding(&padded, width, fill, align, neg, space_sign);
        }
        return Ok(padded);
    }

    let sign_str = if neg {
        "-"
    } else if force_sign {
        "+"
    } else if space_sign {
        " "
    } else {
        ""
    };

    let prec = precision.unwrap_or(0);
    let _ = ctx;

    // Special handling for %: multiply by 100 (exp += 2, preserving the
    // value's exponent representation for trailing-zero semantics).
    let dec = if ty == '%' {
        let scaled = Dec { sign: d.sign, coeff: d.coeff.clone(), exp: d.exp + 2, special: Special::Finite };
        scaled
    } else {
        d.clone()
    };

    let no_type = type_char.is_none();
    let rounding = ctx.rounding;
    let value_str = match ty.to_ascii_lowercase() {
        'f' => format_fixed(&dec, precision, rounding, vm),
        'e' => format_sci(&dec, precision, ty == 'E', rounding, vm),
        'g' | 'n' => {
            if (no_type || ty == 'n' || ty == 'N') && precision.is_none() {
                // No type char / 'n' without precision: Decimal keeps its
                // exact str representation (trailing zeros preserved), like
                // format(d, '') == str(d). Strip the sign (sign_str is
                // applied separately below).
                // Honour context.capitals for the 'E'/'e' exponent marker.
                let mut s = dec.to_string();
                s = s.trim_start_matches(['-', '+']).to_string();
                if capitals == 0 {
                    s = s.replace('E', "e");
                }
                s
            } else {
                format_general(&dec, precision, ty == 'G' || ty == 'N', rounding, vm)
            }
        }
        '%' => {
            // Multiply by 100 was done above; format as f with given
            // precision (or exact value when no precision given).
            let f = format_fixed(&dec, precision, rounding, vm);
            f + "%"
        }
        _ => dec.to_string(),
    };

    let mut signed = format!("{sign_str}{value_str}");
    // Locale formatting: when a localeconv-style dict was supplied (used by
    // the 'n' type / grouping), swap the decimal point and apply the locale
    // thousands grouping to the integer part. '0' zero-padding must be
    // applied to the raw integer digits BEFORE grouping so padded zeros are
    // grouped too (e.g. '08n' of 12345 -> '0,012,345').
    let use_locale =
        !locale.decimal_point.is_empty() || !locale.thousands_sep.is_empty();
    if use_locale && !d.is_special() && (ty == 'n' || grouping.is_none()) {
        let sign_len = if signed.starts_with(['+', '-', ' ']) { 1 } else { 0 };
        let sign = &signed[..sign_len];
        let body = &signed[sign_len..];
        let (int_part, rest) = match body.find(['.', 'e', 'E', '%']) {
            Some(i) => (&body[..i], &body[i..]),
            None => (body, ""),
        };
        // Pre-pad the integer digits with zeros when '0' fill is requested.
        // Add zeros until the grouped output (sign + grouped int + rest)
        // reaches the requested width (CPython pads then groups so the
        // separator chars count toward the width).
        let mut int_digits = int_part.to_string();
        if fill == '0' && width > 0 && !locale.thousands_sep.is_empty() && !rest.starts_with(['e', 'E']) {
            let cur_len = |d: &str| -> usize {
                sign_len + apply_locale_grouping(d, locale).chars().count() + rest.chars().count()
            };
            while cur_len(&int_digits) < width {
                int_digits = format!("0{int_digits}");
                if cur_len(&int_digits) > width {
                    // Only overshoot if a separator push happened; stop there.
                    break;
                }
            }
        }
        let localized = apply_locale_grouping(&int_digits, locale);
        let localized = format!("{localized}{rest}");
        // swap the locale decimal point
        let localized = if !locale.decimal_point.is_empty() {
            localized.replace('.', &locale.decimal_point)
        } else {
            localized
        };
        signed = format!("{sign}{localized}");
    }
    // 'z' option: if the FORMATTED value (after rounding) is negative zero,
    // drop the minus sign (e.g. '-.01' with '.1f' rounds to '-0.0' -> '0.0').
    if coerce_negzero && sign_str == "-" {
        let body = signed.trim_start_matches('-');
        let mantissa: String = body
            .split(['e', 'E', '%'])
            .next()
            .unwrap_or("")
            .chars()
            .filter(|&c| c != '.' && c != ',')
            .collect();
        if !mantissa.is_empty() && mantissa.chars().all(|c| c == '0') {
            signed = signed.replacen('-', "", 1);
        }
    }
    // '#' alternate form: force a decimal point on f/F (and no-type) output.
    if alternate && (ty.to_ascii_lowercase() == 'f' || no_type)
        && !signed.contains(['.', 'e', 'E', '%'])
    {
        signed.push('.');
    }
    // Fraction-only grouping (spec like '.,' or '._'): no integer grouping,
    // only group the fractional digits.
    if grouping.is_none() && frac_sep.is_some() && !d.is_special() {
        let sign_len = if signed.starts_with(['+', '-', ' ']) { 1 } else { 0 };
        let sign = &signed[..sign_len];
        let body = &signed[sign_len..];
        signed = format!("{sign}{}", apply_frac_grouping(body, frac_sep.unwrap()));
    }
    if grouping.is_some() && !d.is_special() {
        // Group first, then handle zero-padding so leading zeros are not
        // drawn into the grouping (e.g. '+09,' of 123456 -> '+0,123,456').
        let sign_len = if signed.starts_with(['+', '-', ' ']) { 1 } else { 0 };
        let sign = &signed[..sign_len];
        let body = &signed[sign_len..];
        let (int_part, rest) = match body.find(['.', 'e', 'E', '%']) {
            Some(i) => (&body[..i], &body[i..]),
            None => (body, ""),
        };
        let cur_digits = int_part.chars().count();
        // Apply integer grouping, then (if a fractional separator is present)
        // group the fractional digits too.
        let group_all = |s: &str| -> String {
            let sep = grouping.unwrap();
            let g = apply_grouping(s, sep);
            match frac_sep {
                Some(fs) => apply_frac_grouping(&g, fs),
                None => g,
            }
        };
        if fill == '0' && width > signed.chars().count() {
            // Only pad when the negatively-grouped total still falls short
            // (fractional parts already widen the string; e.g. '08,' of
            // 1234.56 -> '1,234.56' with no zero pad).
            let cur_grouped = cur_digits.saturating_sub(1) / 3;
            let cur_total = cur_digits + cur_grouped + rest.chars().count();
            let target = width - sign_len;
            if cur_total < target {
                let mut dcount = cur_digits;
                while dcount + (dcount.saturating_sub(1)) / 3 + rest.chars().count() < target {
                    dcount += 1;
                }
                let pad = dcount - cur_digits;
                let padded_digits = format!("{}{}", "0".repeat(pad), int_part);
                signed = format!("{sign}{}{rest}", group_all(&padded_digits));
            } else {
                signed = format!("{sign}{}{rest}", group_all(int_part));
            }
            Ok(signed)
        } else {
            let grouped = group_all(body);
            if width > (sign_len + grouped.chars().count()) {
                let pad = width - sign_len - grouped.chars().count();
                signed = match align {
                    '<' => format!("{sign}{grouped}{}", " ".repeat(pad)),
                    '^' => {
                        let left = pad / 2;
                        format!("{}{}{}{}", " ".repeat(left), sign, grouped, " ".repeat(pad - left))
                    }
                    _ => format!("{}{}{}", " ".repeat(pad), sign, grouped),
                };
            } else {
                signed = format!("{sign}{grouped}");
            }
            Ok(signed)
        }
    } else {
        // Apply width padding with fill/alignment.
        if width > signed.chars().count() {
            signed = apply_padding(&signed, width, fill, align, d.sign, space_sign);
        }
        Ok(signed)
    }
}

fn apply_grouping(s: &str, sep: char) -> String {
    // Group the integer part (before the decimal point / e) in 3s.
    let (int_part, rest) = match s.find(['.', 'e', 'E', '%']) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    // Strip leading sign for grouping count.
    let has_sign = int_part.starts_with(['+', '-', ' ']);
    let digits = if has_sign { &int_part[1..] } else { int_part };
    let mut grouped = String::new();
    let dl = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (dl - i) % 3 == 0 {
            grouped.push(sep);
        }
        grouped.push(c);
    }
    let mut out = String::new();
    if has_sign {
        out.push(int_part.chars().next().unwrap());
    }
    out.push_str(&grouped);
    out.push_str(rest);
    out
}

/// Group the fractional digits after a decimal point using `frac_sep`,
/// inserting the separator every 3 digits from the LEFT
/// (e.g. "23456789" with ',' -> "234,567,89").
fn apply_frac_grouping(s: &str, frac_sep: char) -> String {
    // s like "1.23456789e+5" -> split fraction "23456789" then exponent.
    let dot = s.find('.');
    let Some(dot) = dot else { return s.to_string() };
    let after_dot = &s[dot + 1..];
    let (frac_digits, suffix) = match after_dot.find(['e', 'E']) {
        Some(i) => (&after_dot[..i], &after_dot[i..]),
        None => (after_dot, ""),
    };
    // only group full groups of 3 from the left (leave rightmost partial)
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    let len = frac_digits.len();
    while i < len {
        let take = 3.min(len - i);
        parts.push(frac_digits[i..i + take].to_string());
        i += take;
    }
    let grouped = parts.join(&format!("{frac_sep}"));
    format!("{}{}{}", &s[..dot + 1], grouped, suffix)
}

/// Apply locale grouping to the integer part of a formatted string.
/// `grouping` is the locale grouping byte code: a sequence of group sizes
/// from right to left; 0 means "repeat the previous size"; 127 (CHAR_MAX)
/// means "no more grouping". An empty thousands_sep disables grouping.
fn apply_locale_grouping(body: &str, loc: &Locale) -> String {
    let (int_part, rest) = match body.find(['.', 'e', 'E', '%']) {
        Some(i) => (&body[..i], &body[i..]),
        None => (body, ""),
    };
    let has_sign = int_part.starts_with(['+', '-', ' ']);
    let digits = if has_sign { &int_part[1..] } else { int_part };
    let mut grouped;
    if loc.thousands_sep.is_empty() {
        // No thousands separator: no grouping of the integer part.
        grouped = digits.to_string();
    } else {
        // Interpret the grouping byte code from the right.
        //  - a positive size N groups N digits
        //  - 0 means "repeat the previous size"
        //  - >= 127 (CHAR_MAX) means "no more grouping"
        // If the array ends (or ends in 0), the last positive size repeats;
        // if it ends in CHAR_MAX the remaining digits stay as one group.
        // Build the sequence of group sizes first.
        let mut sizes: Vec<usize> = Vec::new();
        let mut stop_at_end = true;
        for (i, &g) in loc.grouping.iter().enumerate() {
            if g > 0 && g < 127 {
                sizes.push(g as usize);
            } else if g == 0 {
                // repeat the previous size
                let prev = sizes.last().copied().unwrap_or(3);
                sizes.push(prev);
            } else {
                // CHAR_MAX: stop after this point
                stop_at_end = false;
                // the current size (if a positive follows) is not grouped
                let _ = i;
                break;
            }
        }
        let last = sizes.last().copied().unwrap_or(3);
        let mut rem = digits;
        let mut out_s = String::new();
        let mut si = 0;
        loop {
            // Determine the group size for this iteration.
            let sz = if si < sizes.len() {
                sizes[si]
            } else if stop_at_end {
                // Repeat the last size.
                last
            } else {
                // CHAR_MAX: remaining digits are one group.
                if !out_s.is_empty() {
                    out_s = format!("{}{}{}", rem, loc.thousands_sep, out_s);
                } else {
                    out_s = rem.to_string();
                }
                break;
            };
            if rem.len() <= sz {
                if !out_s.is_empty() {
                    out_s = format!("{}{}{}", rem, loc.thousands_sep, out_s);
                } else {
                    out_s = rem.to_string();
                }
                break;
            }
            let split = rem.len() - sz;
            let (head, tail) = rem.split_at(split);
            out_s = if out_s.is_empty() {
                tail.to_string()
            } else {
                format!("{}{}{}", tail, loc.thousands_sep, out_s)
            };
            rem = head;
            si += 1;
        }
        grouped = out_s;
    }
    let mut out = String::new();
    if has_sign {
        out.push(int_part.chars().next().unwrap());
    }
    out.push_str(&grouped);
    if !loc.decimal_point.is_empty() {
        out.push_str(&rest.replace('.', &loc.decimal_point));
    } else {
        out.push_str(rest);
    }
    out
}

fn apply_padding(s: &str, width: usize, fill: char, align: char, neg: bool, space: bool) -> String {
    let len = s.chars().count();
    let pad = width - len;
    if align == '=' {
        // '=' align: pad between the sign and the number.
        let prefix = if s.starts_with(['+', '-', ' ']) { 1 } else { 0 };
        let (head, tail) = s.split_at(prefix);
        format!("{}{}{}", head, fill.to_string().repeat(pad), tail)
    } else if fill == '0' {
        // zero-pad: insert zeros right after the sign.
        let prefix = if s.starts_with(['+', '-', ' ']) { 1 } else { 0 };
        let (head, tail) = s.split_at(prefix);
        format!("{}{}{}", head, "0".repeat(pad), tail)
    } else {
        match align {
            '<' => format!("{}{}", s, fill.to_string().repeat(pad)),
            '^' => {
                let left = pad / 2;
                let right = pad - left;
                format!("{}{}{}", fill.to_string().repeat(left), s, fill.to_string().repeat(right))
            }
            _ => format!("{}{}", fill.to_string().repeat(pad), s),
        }
    }
}

fn digits_of(d: &Dec) -> String {
    d.coeff_str()
}

fn round_to_exp(d: &Dec, exp: isize, rounding: Rounding, vm: &VirtualMachine) -> Dec {
    // Round d to the given exponent using the given rounding mode
    // (defaults to the context's, callers pass ctx.rounding).
    let ctx = Ctx::from_parts(28, rounding, 999999, -999999, 0, vm.ctx.new_dict(), vm.ctx.new_dict());
    let r = quantize(vm, &ctx, d, exp);
    match r {
        Ok(dec) => dec,
        Err(_) => d.clone(),
    }
}

fn format_fixed(d: &Dec, precision: Option<usize>, rounding: Rounding, vm: &VirtualMachine) -> String {
    // Fixed point: value = coeff * 10^exp, show `precision` digits after point.
    let digits = digits_of(d);
    let ndigits = digits.len() as isize;
    let adjusted = d.exp + ndigits - 1;
    // Target exponent: with precision p -> -p; without precision -> keep
    // the value's own exponent (preserve trailing zeros).
    let target_exp = match precision {
        Some(p) => -(p as isize),
        None => d.exp.min(0),
    };
    let frac_digits = precision.unwrap_or(0) as isize;
    let rounded = round_to_exp(d, target_exp, rounding, vm);
    let rd = digits_of(&rounded);
    // Place the decimal point.
    let rn = rd.len() as isize;
    let point_pos = rn + rounded.exp;
    if point_pos <= 0 {
        // 0.xxxx
        let mut s = String::from("0.");
        let zeros = (-point_pos) as usize;
        s.push_str(&"0".repeat(zeros));
        s.push_str(&rd);
        if let Some(p) = precision {
            // pad to requested precision
            let total = zeros + rn as usize;
            if total < p {
                s.push_str(&"0".repeat(p - total));
            }
        }
        s
    } else if point_pos >= rn {
        // integer with trailing zeros
        let mut s = rd.clone();
        let zeros = (point_pos - rn) as usize;
        s.push_str(&"0".repeat(zeros));
        if frac_digits > 0 {
            s.push('.');
            s.push_str(&"0".repeat(frac_digits as usize));
        }
        s
    } else {
        let ip = point_pos as usize;
        let mut s = String::from(&rd[..ip]);
        s.push('.');
        s.push_str(&rd[ip..]);
        if let Some(p) = precision {
            let cur_frac = rn as usize - ip;
            if cur_frac < p {
                s.push_str(&"0".repeat(p - cur_frac));
            }
        }
        s
    }
}

fn format_sci(d: &Dec, precision: Option<usize>, upper: bool, rounding: Rounding, vm: &VirtualMachine) -> String {
    let digits = digits_of(d);
    if digits == "0" {
        // Zero: exponent for 'e' format is stored_exp + precision
        // (CPython: .6e of 0E-15 -> 0.000000e-9, of 0 -> 0.000000e+6).
        let p = precision.unwrap_or(0) as isize;
        let e = d.exp + p;
        let e_str = format!("{}{}", if upper { "E" } else { "e" }, if e >= 0 { "+" } else { "" });
        let mut s = String::from("0");
        if p > 0 {
            s.push('.');
            s.push_str(&"0".repeat(p as usize));
        }
        s.push_str(&e_str);
        s.push_str(&e.to_string());
        return s;
    }
    // adjusted exponent
    let adj = d.exp + digits.len() as isize - 1;
    match precision {
        // With precision `p`: p+1 sig figs, p digits after the point.
        Some(p) => {
            let p = p as isize;
            let keep = p + 1;
            let n_digits = digits.len() as isize;
            let mut sig;
            let mut new_adj = adj;
            if n_digits > keep {
                let drop = n_digits - keep;
                let pow = BigInt::from(10).pow(drop as u32);
                let coeff: BigInt = digits.parse().unwrap_or_else(|_| BigInt::from(0));
                let (q, r) = (&coeff / &pow, &coeff % &pow);
                // Respect the context rounding mode (fixes
                // test_format_fallback_rounding with ROUND_DOWN).
                let two = BigInt::from(2);
                let round_up = match rounding {
                    Rounding::Down => false,
                    // Directed rounding only moves the result when the discarded
                    // digits are non-zero; an all-zero remainder is already exact.
                    Rounding::Up => r != BigInt::from(0),
                    Rounding::Ceiling => !d.sign && r != BigInt::from(0),
                    Rounding::Floor => d.sign && r != BigInt::from(0),
                    Rounding::HalfDown => (&r * &two) > pow,
                    Rounding::HalfUp => (&r * &two) >= pow,
                    Rounding::HalfEven => {
                        if (&r * &two) > pow { true }
                        else if (&r * &two) < pow { false }
                        else { (&q % &two) == BigInt::from(1) }
                    }
                    Rounding::Half05Up => {
                        if r == BigInt::from(0) { false }
                        else { (&q % BigInt::from(10)) == BigInt::from(0) || (&q % BigInt::from(10)) == BigInt::from(5) }
                    }
                    Rounding::Half01Up => {
                        if r == BigInt::from(0) { false }
                        else { (&q % BigInt::from(10)) == BigInt::from(0) || (&q % BigInt::from(10)) == BigInt::from(1) }
                    }
                };
                let new_q = if round_up { q + BigInt::from(1) } else { q };
                sig = new_q.to_string();
                new_adj = adj + (sig.len() as isize - keep);
                // A round-carry may add a digit (99... -> 100...): trim sig
                // back to `keep` chars (the exponent already absorbed it).
                if sig.len() as isize > keep {
                    sig.truncate(keep as usize);
                }
            } else {
                sig = digits.clone();
                // pad with zeros to exactly keep digits
                while (sig.len() as isize) < keep {
                    sig.push('0');
                }
            }
            format_mantissa(&sig, p, upper, new_adj)
        }
        // No precision: preserve all significant digits.
        None => format_mantissa(&digits, digits.len() as isize - 1, upper, adj),
    }
}

fn format_mantissa(sig: &str, precision: isize, upper: bool, adj: isize) -> String {
    let e_str = format!("{}{}", if upper { "E" } else { "e" }, if adj >= 0 { "+" } else { "" });
    let mut s = String::new();
    let first = sig.chars().next().unwrap_or('0');
    s.push(first);
    if precision > 0 {
        s.push('.');
        let frac = if sig.len() > 1 { &sig[1..] } else { "" };
        s.push_str(frac);
        let gap = precision as isize - frac.len() as isize;
        if gap > 0 {
            s.push_str(&"0".repeat(gap as usize));
        }
    }
    s.push_str(&e_str);
    s.push_str(&adj.to_string());
    s
}

fn format_general(d: &Dec, precision: Option<usize>, _upper: bool, rounding: Rounding, vm: &VirtualMachine) -> String {
    // General format: like 'g'. Uses precision sig figs (default 6).
    let p = precision.unwrap_or(6).max(1);
    let digits = digits_of(d);
    if digits == "0" {
        // zero: keep fractional zeros for small exponents
        // (0.0 -> '0.0', 0E-5 -> '0.00000'), scientific when the fractional
        // zeros would exceed the precision (0E-7 -> '0e-7' with p=6) or the
        // exponent is positive (0E1 -> '0e+1').
        let zoom = -d.exp;
        let e_marker = if _upper { "E" } else { "e" };
        if d.exp > 0 {
            return format!("0{e_marker}+{}", d.exp);
        }
        if zoom > 0 && zoom <= p as isize {
            // 0.0 / 0.00 ... show with the fractional zeros
            let mut s = String::from("0.");
            s.push_str(&"0".repeat(zoom as usize));
            return s;
        }
        if zoom > p as isize {
            return format!("0{e_marker}-{}", zoom);
        }
        return "0".to_string();
    }
    let adj = d.exp + digits.len() as isize - 1;
    // g switches to scientific if adj < -4 or adj >= p
    if adj < -4 || adj >= p as isize {
        // scientific, strip trailing zeros from the significant digits
        let s = format_sci(d, Some((p - 1) as usize), _upper, rounding, vm);
        if _upper {
            s.replace('e', "E")
        } else {
            strip_sci_trailing_zeros(&s)
        }
    } else {
        // fixed with p - (adj+1) digits after point, strip trailing zeros
        let frac = p as isize - (adj + 1);
        let s = format_fixed(d, Some(frac.max(0) as usize), rounding, vm);
        strip_fixed_trailing_zeros(&s)
    }
}

/// Remove trailing zeros from the mantissa of a scientific-notation string
/// (keeps at least one fraction digit if present).
fn strip_sci_trailing_zeros(s: &str) -> String {
    // s looks like "1.2300000e+5"
    let (mantissa, exp) = match s.find(['e', 'E']) {
        Some(idx) => (&s[..idx], &s[idx..]),
        None => return s.to_string(),
    };
    let mut m = mantissa.to_string();
    if let Some(dot) = m.find('.') {
        while m.len() > dot + 2 && m.ends_with('0') {
            m.pop();
        }
        // keep trailing (1.0 -> 1)? CPython keeps "3." for g? No: strips.
        if m.ends_with('.') {
            m.pop();
        }
    }
    format!("{}{}", m, exp)
}

/// Remove trailing zeros from the fractional part of a fixed string.
fn strip_fixed_trailing_zeros(s: &str) -> String {
    if s.contains('.') {
        let t = s.trim_end_matches('0');
        let t = t.trim_end_matches('.');
        if t.is_empty() { "0".to_string() } else { t.to_string() }
    } else {
        s.to_string()
    }
}


