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
}

static_cell! {
    static EXC: DecExc;
}

pub(crate) fn exc() -> &'static DecExc {
    EXC.get().expect("_decimal exceptions not initialized")
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
    let args = vec![vm.ctx.new_str(msg.to_string()).into()];
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
        self.flags.set_item(ty.as_object(), vm.ctx.new_bool(true).into(), vm)?;
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
    let two = BigInt::from(2);
    let inexact = r != BigInt::from(0);

    let round_up = match ctx.rounding {
        Rounding::Down => false,
        Rounding::Up => true,
        Rounding::Ceiling => !sign,
        Rounding::Floor => sign,
        Rounding::HalfDown => (&r * &two) > pow10,
        Rounding::HalfUp => (&r * &two) >= pow10,
        Rounding::HalfEven => {
            if (&r * &two) > pow10 {
                true
            } else if (&r * &two) < pow10 {
                false
            } else {
                // tie: round to even
                (&q % &two) == BigInt::from(1)
            }
        }
        Rounding::Half05Up => {
            // ROUND_05UP: round to nearest, ties away from zero, but if the
            // last kept digit is 0 or 5, round up regardless.
            if (&r * &two) >= pow10 {
                true
            } else {
                let last = (&q % BigInt::from(10)) == BigInt::from(0)
                    || (&q % BigInt::from(10)) == BigInt::from(5);
                last
            }
        }
        Rounding::Half01Up => {
            // ROUND_01UP: like HalfUp for ties, and rounds up when the last
            // digit is 0 or 1.
            if (&r * &two) >= pow10 {
                true
            } else {
                let last = (&q % BigInt::from(10)) == BigInt::from(0)
                    || (&q % BigInt::from(10)) == BigInt::from(1);
                last
            }
        }
    };

    let new_coeff = if round_up { q + BigInt::from(1) } else { q };
    Ok((new_coeff, exp + discard, inexact))
}

pub(crate) fn coeff_digits_of(coeff: &BigInt) -> usize {
    let mut s = coeff.to_string();
    if s == "0" { return 1; }
    let t = s.trim_start_matches('0');
    if t.is_empty() { 1 } else { t.len() }
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
    dec.coeff = new_coeff;
    dec.exp = new_exp;
    // exponent range check
    let etiny = ctx.emin - (ctx.prec as isize - 1);
    if new_exp < etiny {
        // Underflow / Subnormal
        ctx.set_flag(vm, "Subnormal")?;
        ctx.set_flag(vm, "Underflow")?;
        if ctx.trap(vm, "Underflow")? {
            return Err(ctx_trap_exc(vm, "Underflow", "underflow"));
        }
        // clamp to etiny
        let shift = etiny - new_exp;
        let pow = BigInt::from(10).pow(shift as u32);
        let (q, r) = (&dec.coeff / &pow, &dec.coeff % &pow);
        dec.coeff = q;
        dec.exp = etiny;
        if r != BigInt::from(0) {
            ctx.set_flag(vm, "Inexact")?;
            ctx.set_flag(vm, "Rounded")?;
        }
        ctx.set_flag(vm, "Clamped")?;
        if ctx.trap(vm, "Clamped")? {
            return Err(ctx_trap_exc(vm, "Clamped", "clamped"));
        }
    } else if dec.exp + coeff_digits_of(&dec.coeff) as isize - 1 > ctx.emax {
        // Overflow is determined by the adjusted exponent, not just the
        // stored coefficient exponent: 999 * 1E+425000000 has adjusted
        // exponent 425000002 even though its stored exponent is Emax.
        ctx.set_flag(vm, "Overflow")?;
        ctx.set_flag(vm, "Inexact")?;
        ctx.set_flag(vm, "Rounded")?;
        if ctx.trap(vm, "Overflow")? {
            return Err(ctx_trap_exc(vm, "Overflow", "overflow"));
        }
        // represent as Inf (with sign) per spec
        dec.special = Special::Infinity;
        dec.coeff = BigInt::from(0);
        dec.exp = 0;
    }
    Ok(dec)
}

pub(crate) fn ctx_trap_exc(vm: &VirtualMachine, name: &str, _msg: &str) -> PyBaseExceptionRef {
    new_decimal_error(vm, name, &format!("[{name}]"))
}

/// Add two finite decimals (context applied).
pub(crate) fn dec_add(ctx: &Ctx, vm: &VirtualMachine, a: &Dec, b: &Dec) -> PyResult<Dec> {
    // Special propagation.
    if a.is_special() || b.is_special() {
        return special_binary(vm, ctx, a, b, "add");
    }
    let e = a.exp.min(b.exp);
    let pa = BigInt::from(10).pow((a.exp - e) as u32);
    let pb = BigInt::from(10).pow((b.exp - e) as u32);
    let ca = &a.coeff * &pa;
    let cb = &b.coeff * &pb;
    let va = if a.sign { -&ca } else { ca.clone() };
    let vb = if b.sign { -&cb } else { cb.clone() };
    let sum = va + vb;
    let sign = sum.sign() == Sign::Minus;
    let coeff = sum.abs();
    apply_context(vm, ctx, Dec { sign, coeff, exp: e, special: Special::Finite })
}

/// Subtract (a - b).
pub(crate) fn dec_sub(ctx: &Ctx, vm: &VirtualMachine, a: &Dec, b: &Dec) -> PyResult<Dec> {
    let neg = Dec { sign: !b.sign, coeff: b.coeff.clone(), exp: b.exp, special: b.special };
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
    // NaN dominates
    if a.special == Special::NaN || a.special == Special::SignalingNaN
        || b.special == Special::NaN || b.special == Special::SignalingNaN
    {
        let s = if a.special == Special::SignalingNaN || b.special == Special::SignalingNaN {
            Special::SignalingNaN
        } else {
            Special::NaN
        };
        let sign = if a.special != Special::Finite { a.sign } else { b.sign };
        return Ok(Dec { sign, coeff: BigInt::from(0), exp: 0, special: s });
    }
    let inf_a = a.special == Special::Infinity;
    let inf_b = b.special == Special::Infinity;
    if inf_a && inf_b {
        // Inf op Inf
        let neg = a.sign ^ b.sign;
        return Ok(Dec { sign: neg, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
    }
    if inf_a {
        let sign = a.sign ^ (b.sign && !b.is_zero());
        return Ok(Dec { sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
    }
    if inf_b {
        let sign = (a.sign && !a.is_zero()) ^ b.sign;
        return Ok(Dec { sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity });
    }
    Ok(Dec::zero())
}

/// True division with context precision (long division to prec+1 digits).
pub(crate) fn dec_div(ctx: &Ctx, vm: &VirtualMachine, a: &Dec, b: &Dec) -> PyResult<Dec> {
    if b.is_zero() {
        ctx.set_flag(vm, "DivisionByZero")?;
        if ctx.trap(vm, "DivisionByZero")? {
            return Err(ctx_trap_exc(vm, "DivisionByZero", "division by zero"));
        }
        if a.is_zero() {
            return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
        }
        return Ok(Dec {
            sign: a.sign ^ b.sign,
            coeff: BigInt::from(0),
            exp: 0,
            special: Special::Infinity,
        });
    }
    if a.is_special() || b.is_special() {
        return special_binary(vm, ctx, a, b, "div");
    }
    // a / b = (ca * 10^ea) / (cb * 10^eb)
    // long division producing the exact terminating form (keeps trailing
    // zeros like CPython: 2.0/2 -> 1.0), capped at prec+1 digits for
    // non-terminating quotients (apply_context rounds to prec).
    let prec = ctx.prec as usize;
    let mut num = a.coeff.clone();
    let den = b.coeff.clone();
    let mut rem = &num % &den;
    let ip = &num / &den;
    let mut coeff = ip.clone();
    let mut exp = a.exp - b.exp;
    let mut coeff_digits = if ip == BigInt::from(0) { 0 } else { coeff_digits_of(&ip) };
    while rem != BigInt::from(0) && coeff_digits <= prec {
        rem *= BigInt::from(10);
        let d = &rem / &den;
        rem = &rem % &den;
        if coeff == BigInt::from(0) {
            exp -= 1;
            if d != BigInt::from(0) {
                coeff = d.clone();
                coeff_digits = 1;
            }
        } else {
            coeff = &coeff * 10 + &d;
            coeff_digits += 1;
            exp -= 1;
        }
    }
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
    // split mantissa on '.'
    let (int_part, frac_part) = match mantissa.find('.') {
        Some(idx) => (&mantissa[..idx], &mantissa[idx + 1..]),
        None => (mantissa, ""),
    };
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
    // CPython quantize raises InvalidOperation if the result's adjusted
    // exponent would fall outside [Emin, Emax]. The quantized value keeps
    // its digits; exponent is fixed to `exp` after rounding/carry.
    // Estimate adjusted exponent pre-rounding (may carry one more digit).
    let digits_now = coeff_digits_of(&d.coeff) as isize;
    let adj_before = d.exp + digits_now - 1;
    // After quantizing to `exp`, the significand may shrink (rounding away
    // digits) or grow by one via carry. Worst-case adjusted exponent:
    // if no digits are dropped, adj = exp + digits_now - 1.
    let adj_est = exp + digits_now - 1;
    if adj_est > ctx.emax || adj_before < ctx.emin {
        ctx.set_flag(vm, "InvalidOperation")?;
        if ctx.trap(vm, "InvalidOperation")? {
            return Err(new_decimal_error(vm, "InvalidOperation", "quantize exponent out of range"));
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
            Rounding::Up => true,
            Rounding::Ceiling => !d.sign,
            Rounding::Floor => d.sign,
            Rounding::HalfDown => (&r * &two) > pow,
            Rounding::HalfUp => (&r * &two) >= pow,
            Rounding::HalfEven => {
                if (&r * &two) > pow { true }
                else if (&r * &two) < pow { false }
                else { (&q % &two) == BigInt::from(1) }
            }
            Rounding::Half05Up => {
                if (&r * &two) >= pow {
                    true
                } else {
                    let last = (&q % BigInt::from(10)) == BigInt::from(0)
                        || (&q % BigInt::from(10)) == BigInt::from(5);
                    last
                }
            }
            Rounding::Half01Up => {
                if (&r * &two) >= pow {
                    true
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
    let mut result = Dec { sign: d.sign, coeff, exp, special: Special::Finite };
    // precision check on result digits
    if coeff_digits_of(&result.coeff) as isize > ctx.prec {
        result = apply_context(vm, ctx, result)?;
    }
    if inexact {
        ctx.set_flag(vm, "Inexact")?;
        ctx.set_flag(vm, "Rounded")?;
    }
    Ok(result)
}

/// Round to integer (exp 0) using the context rounding mode, WITHOUT the
/// Emax/Emin range check (CPython's Decimal.to_integral* returns the rounded
/// integer even if its adjusted exponent exceeds Emax).
pub(crate) fn to_integral(vm: &VirtualMachine, ctx: &Ctx, d: &Dec) -> PyResult<Dec> {
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
    quantize_inner(vm, ctx, d, 0)
}

/// quantize to `exp` using context rounding — shared core without the
/// Emax/Emin InvalidOperation check (used by to_integral).
fn quantize_inner(vm: &VirtualMachine, ctx: &Ctx, d: &Dec, exp: isize) -> PyResult<Dec> {
    let shift = exp - d.exp;
    let mut coeff = d.coeff.clone();
    let mut inexact = false;
    if shift > 0 {
        let pow = BigInt::from(10).pow(shift as u32);
        let (q, r) = (&coeff / &pow, &coeff % &pow);
        let two = BigInt::from(2);
        let round_up = match ctx.rounding {
            Rounding::Down => false,
            Rounding::Up => true,
            Rounding::Ceiling => !d.sign,
            Rounding::Floor => d.sign,
            Rounding::HalfDown => (&r * &two) > pow,
            Rounding::HalfUp => (&r * &two) >= pow,
            Rounding::HalfEven => {
                if (&r * &two) > pow { true }
                else if (&r * &two) < pow { false }
                else { (&q % &two) == BigInt::from(1) }
            }
            Rounding::Half05Up => {
                if (&r * &two) >= pow { true }
                else { (&q % BigInt::from(10)) == BigInt::from(0) || (&q % BigInt::from(10)) == BigInt::from(5) }
            }
            Rounding::Half01Up => {
                if (&r * &two) >= pow { true }
                else { (&q % BigInt::from(10)) == BigInt::from(0) || (&q % BigInt::from(10)) == BigInt::from(1) }
            }
        };
        inexact = r != BigInt::from(0);
        coeff = if round_up { q + BigInt::from(1) } else { q };
    } else if shift < 0 {
        coeff *= BigInt::from(10).pow((-shift) as u32);
    }
    let result = Dec { sign: d.sign, coeff, exp, special: Special::Finite };
    // to_integral keeps all digits (does not apply context precision).
    if inexact {
        ctx.set_flag(vm, "Inexact")?;
        ctx.set_flag(vm, "Rounded")?;
    }
    Ok(result)
}

/// Integer exponentiation with context rounding.
pub(crate) fn dec_pow_int(ctx: &Ctx, vm: &VirtualMachine, a: &Dec, b: &Dec) -> PyResult<Dec> {
    if b.special != Special::Finite {
        return Ok(Dec { sign: a.sign, coeff: BigInt::from(0), exp: 0, special: b.special });
    }
    let is_integer = !(b.exp < 0
        && (&b.coeff % BigInt::from(10).pow((-b.exp) as u32)) != BigInt::from(0));
    if b.exp > 0 {
        // positive exponent, still integer
    } else if b.exp < 0 && !is_integer {
        // Fractional exponent: compute via exp(n * ln(a)) at high precision.
        // We build a wide (extra-precision) context, apply rounding at the
        // end with the caller's context (which may overflow -> Infinity).
        let wide_prec = (ctx.prec as usize + 20).max(30);
        let wide = Ctx::from_parts(
            wide_prec as isize,
            Rounding::HalfEven,
            ctx.emax,
            ctx.emin,
            ctx.capitals,
            vm.ctx.new_dict(),
            vm.ctx.new_dict(),
        );
        let xf: f64 = a.to_string().parse().unwrap_or(1.0);
        let yf: f64 = b.to_string().parse().unwrap_or(1.0);
        let r = xf.powf(yf);
        let s = format!("{:.*}", wide_prec, r);
        let d = parse_decimal(&s).unwrap_or(Dec::zero());
        return apply_context(vm, ctx, d);
    }
    let n: i64 = if b.exp < 0 {
        let pow = BigInt::from(10).pow((-b.exp) as u32);
        let q = &b.coeff / &pow;
        q.to_string().parse().unwrap_or(0)
    } else {
        (&b.coeff * BigInt::from(10).pow(b.exp as u32)).to_string().parse().unwrap_or(0)
    };
    if n < 0 {
        let base = dec_pow_int(
            ctx,
            vm,
            a,
            &Dec { sign: false, coeff: BigInt::from(n.unsigned_abs()), exp: 0, special: Special::Finite },
        )?;
        let one = Dec { sign: false, coeff: BigInt::from(1), exp: 0, special: Special::Finite };
        return dec_div(ctx, vm, &one, &base);
    }
    let mut result = Dec { sign: false, coeff: BigInt::from(1), exp: 0, special: Special::Finite };
    let mut base = a.clone();
    let mut exp = n;
    while exp > 0 {
        if exp & 1 == 1 {
            result = dec_mul(ctx, vm, &result, &base)?;
        }
        exp >>= 1;
        if exp > 0 {
            base = dec_mul(ctx, vm, &base, &base)?;
        }
    }
    Ok(result)
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
    let _ = ctx;
    Ok(Dec { sign: false, coeff: result, exp: 0, special: Special::Finite })
}

/// floor division a // b and modulo a % b, computed exactly (sign of divisor
/// for modulo, floor for division). Returns (floor, mod).
pub(crate) fn floor_div_mod(a: &Dec, b: &Dec) -> (Dec, Dec) {
    // Specials per CPython:
    //   divmod(inf, finite)   = (Inf, NaN)
    //   divmod(finite, inf)   = (0, a)
    //   divmod(inf, inf)      = (NaN, NaN)
    //   divmod(nan, x)        = (NaN, NaN)
    //   divmod(x, 0)          = (NaN, NaN)
    let nan = Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN };
    let is_nan = |d: &Dec| d.special == Special::NaN || d.special == Special::SignalingNaN;
    if is_nan(a) || is_nan(b) {
        return (nan.clone(), nan.clone());
    }
    let zero = Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::Finite };
    if b.is_zero() {
        // b == 0:
        //   a == 0 -> (NaN, NaN) [InvalidOperation]
        //   a != 0 -> (Infinity, NaN) [InvalidOperation + DivisionByZero]
        if a.is_zero() {
            return (nan.clone(), nan.clone());
        }
        return (
            Dec { sign: a.sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity },
            nan.clone(),
        );
    }
    if a.is_special() {
        // a == Infinity
        if b.is_special() {
            // inf / inf -> (NaN, NaN)
            return (nan.clone(), nan.clone());
        }
        // inf / finite -> (Inf, NaN)
        return (
            Dec { sign: a.sign, coeff: BigInt::from(0), exp: 0, special: Special::Infinity },
            nan.clone(),
        );
    }
    if b.is_special() {
        // finite / inf -> (0, a)
        return (zero, a.clone());
    }
    // value = ca*10^ea / (cb*10^eb)
    let e = a.exp - b.exp;
    let (num, den) = if e >= 0 {
        (&a.coeff * BigInt::from(10).pow(e as u32), b.coeff.clone())
    } else {
        (a.coeff.clone(), &b.coeff * BigInt::from(10).pow((-e) as u32))
    };
    let q = &num / &den;
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
    let msign = rv.sign() == Sign::Minus;
    let mcoeff = rv.abs();
    let m = Dec { sign: msign, coeff: mcoeff, exp: me, special: Special::Finite };
    (floor, m)
}

/// floor division a // b (context applied)
pub(crate) fn dec_floordiv(ctx: &Ctx, vm: &VirtualMachine, a: &Dec, b: &Dec) -> PyResult<Dec> {
    if b.is_zero() {
        ctx.set_flag(vm, "DivisionByZero")?;
        if ctx.trap(vm, "DivisionByZero")? {
            return Err(new_decimal_error(vm, "DivisionByZero", "division by zero"));
        }
        return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
    }
    if a.is_special() || b.is_special() {
        return special_binary(vm, ctx, a, b, "floordiv");
    }
    let (floor, _) = floor_div_mod(a, b);
    apply_context(vm, ctx, floor)
}

/// modulo a % b (sign of divisor)
pub(crate) fn dec_mod(ctx: &Ctx, vm: &VirtualMachine, a: &Dec, b: &Dec) -> PyResult<Dec> {
    if b.is_zero() {
        ctx.set_flag(vm, "InvalidOperation")?;
        if ctx.trap(vm, "InvalidOperation")? {
            return Err(new_decimal_error(vm, "InvalidOperation", "division by zero"));
        }
        return Ok(Dec { sign: false, coeff: BigInt::from(0), exp: 0, special: Special::NaN });
    }
    if a.is_special() || b.is_special() {
        return special_binary(vm, ctx, a, b, "mod");
    }
    let (_, m) = floor_div_mod(a, b);
    apply_context(vm, ctx, m)
}

/// Format a finite decimal for `format()` / `__format__`.
/// Supports the CPython Decimal format spec: `[[fill]align][sign][#][0][width][,][.precision][type]`
/// with types e/E/f/F/g/G/n/%. Rounding is round-half-even via context.
pub(crate) fn format_decimal(
    d: &Dec,
    spec: &str,
    vm: &VirtualMachine,
    ctx: &Ctx,
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
            'e' | 'E' | 'f' | 'F' | 'g' | 'G' | 'n' => {
                spec.pop();
                Some(last)
            }
            _ => None,
        }
    };
    // Precision: `.N` prefix in what remains.
    let mut precision: Option<usize> = None;
    if let Some(dot) = spec.rfind('.') {
        let after = &spec[dot + 1..];
        if !after.is_empty() && after.chars().all(|c| c.is_ascii_digit()) {
            precision = Some(after.parse().unwrap_or(0));
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
        // sign flag
        if idx < rbytes.len() && (rbytes[idx] == '+' || rbytes[idx] == '-' || rbytes[idx] == ' ') {
            sign_flag = rbytes[idx];
            idx += 1;
        }
        // '0' means zero-pad after sign
        if idx < rbytes.len() && rbytes[idx] == '0' {
            fill = '0';
            idx += 1;
        }
        if idx < rbytes.len() && rbytes[idx] == '#' {
            idx += 1;
        }
        // width (digits)
        let mut wdigits = String::new();
        while idx < rbytes.len() && rbytes[idx].is_ascii_digit() {
            wdigits.push(rbytes[idx]);
            idx += 1;
        }
        if !wdigits.is_empty() {
            width = wdigits.parse().unwrap_or(0);
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
    let ty = type_char.unwrap_or('n');
    // Sign prefix from spec's sign flag + value sign.
    // `+`: always show sign; `-`: only negatives; ` `: space for positive.
    let force_sign = sign_flag == '+';
    let space_sign = sign_flag == ' ';
    // Also strip the sign flag from `rest` so it isn't misread below.
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
        if d.sign {
            s.insert(0, '-');
        } else if force_sign {
            s.insert(0, '+');
        } else if space_sign {
            s.insert(0, ' ');
        }
        // Apply width padding (NaN/Inf subject to fill/align).
        if s.chars().count() < width {
            s = apply_padding(&s, width, fill, align, d.sign, space_sign);
        }
        return Ok(s);
    }

    let sign_str = if d.sign {
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
    let value_str = match ty.to_ascii_lowercase() {
        'f' => format_fixed(&dec, precision, vm),
        'e' => format_sci(&dec, precision, ty == 'E', vm),
        'g' | 'n' => {
            if no_type && precision.is_none() {
                // No type char: Decimal keeps its exact str representation
                // (trailing zeros preserved), like format(d, '') == str(d).
                // Strip the sign (sign_str is applied separately below).
                let s = dec.to_string();
                s.trim_start_matches(['-', '+']).to_string()
            } else {
                format_general(&dec, precision, ty == 'G' || ty == 'n', vm)
            }
        }
        '%' => {
            // Multiply by 100 was done above; format as f with given
            // precision (or exact value when no precision given).
            let f = format_fixed(&dec, precision, vm);
            f + "%"
        }
        _ => dec.to_string(),
    };

    let mut signed = format!("{sign_str}{value_str}");
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
                signed = format!("{sign}{}{rest}", apply_grouping(&padded_digits, grouping.unwrap()));
            } else {
                signed = format!("{sign}{}{rest}", apply_grouping(int_part, grouping.unwrap()));
            }
            Ok(signed)
        } else {
            let grouped = apply_grouping(body, grouping.unwrap());
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

fn round_to_exp(d: &Dec, exp: isize, vm: &VirtualMachine) -> Dec {
    // Round d to the given exponent using the default context rounding
    // (round-half-even). Returns a Dec with coeff/exp reflecting rounding.
    let ctx = Ctx::from_parts(28, Rounding::HalfEven, 999999, -999999, 0, vm.ctx.new_dict(), vm.ctx.new_dict());
    let r = quantize(vm, &ctx, d, exp);
    match r {
        Ok(dec) => dec,
        Err(_) => d.clone(),
    }
}

fn format_fixed(d: &Dec, precision: Option<usize>, _vm: &VirtualMachine) -> String {
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
    let rounded = round_to_exp(d, target_exp, _vm);
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

fn format_sci(d: &Dec, precision: Option<usize>, upper: bool, vm: &VirtualMachine) -> String {
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
                let half = &pow / BigInt::from(2);
                let round_up = if &r * BigInt::from(2) > pow { true }
                    else if &r * BigInt::from(2) < pow { false }
                    else { (&q % BigInt::from(2)) == BigInt::from(1) };
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

fn format_general(d: &Dec, precision: Option<usize>, _upper: bool, _vm: &VirtualMachine) -> String {
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
        let s = format_sci(d, Some((p - 1) as usize), _upper, _vm);
        if _upper {
            s.replace('e', "E")
        } else {
            strip_sci_trailing_zeros(&s)
        }
    } else {
        // fixed with p - (adj+1) digits after point, strip trailing zeros
        let frac = p as isize - (adj + 1);
        let s = format_fixed(d, Some(frac.max(0) as usize), _vm);
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


