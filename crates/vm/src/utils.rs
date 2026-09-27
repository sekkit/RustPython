use rustpython_common::wtf8::{Wtf8, Wtf8Buf};

use crate::{
    PyObjectRef, PyResult, VirtualMachine,
    builtins::{PyStr, PyUtf8Str},
    convert::{ToPyException, ToPyObject},
};

// Display name of the builtin call currently being dispatched, used by
// `FuncArgs` binding error messages to reproduce CPython's
// `f() takes no keyword arguments` / `f expected at least N arguments, got
// M` phrasing. Set around the actual call in the builtin call funnels
// (PyNativeFunction::call, PyType::call, PyMethodWrapper::call) and restored
// afterwards so nested calls report the innermost function. Plain // comments
// (not /// doc comments) so the lint on the macro-expanded thread_local is not
// triggered.
//
// The slot stores only a borrowed pointer to the callable: the display-name
// string is built lazily, and only when an argument-binding error actually
// needs it. Building it eagerly made every builtin call allocate one or two
// strings (`format!`/`to_owned`) plus a `RefCell` borrow for nothing on the
// success path. The pointer borrows the callable, which the caller owns for
// the entire call (see `with_call_context`).
#[derive(Clone, Copy)]
pub(crate) enum CallContext {
    None,
    Obj(*const crate::PyObject),
}
thread_local! {
    pub(crate) static CURRENT_CALL: core::cell::Cell<CallContext> =
        const { core::cell::Cell::new(CallContext::None) };
    // Number of implicit `self` arguments prepended by bound builtin methods
    // (1) or not (0). CPython's error messages count only the *external*
    // arguments ("keys() takes no arguments (1 given)" for `{}.keys(0)`),
    // so the binding error paths subtract this offset.
    pub(crate) static CURRENT_CALL_SELF_OFFSET: core::cell::Cell<usize> =
        const { core::cell::Cell::new(0) };
}

/// Reads the display name of the currently-running builtin call, if any.
/// Only called from error paths, so resolving the name here is free.
pub(crate) fn current_call_name(vm: &VirtualMachine) -> Option<String> {
    match CURRENT_CALL.with(|c| c.get()) {
        CallContext::None => None,
        // SAFETY: the pointer was installed by `with_call_context`, whose
        // borrow of the callable stays live until the call returns.
        CallContext::Obj(obj) => unsafe {
            crate::protocol::callable_display_name(&*obj, vm)
        },
    }
}

/// Reads how many implicit `self` arguments the current builtin call had
/// prepended (1 for bound methods, 0 otherwise).
pub(crate) fn current_call_self_offset() -> usize {
    CURRENT_CALL_SELF_OFFSET.with(|c| c.get())
}

/// Sets the call context for the duration of `f` and restores the previous
/// values afterwards (nested builtin calls override and then restore).
///
/// `callable` is borrowed for the whole call, so the raw pointer stored in
/// the thread-local is valid whenever `f` (or anything it calls) reads it.
pub(crate) fn with_call_context<T>(
    callable: Option<&crate::PyObject>,
    self_offset: usize,
    f: impl FnOnce() -> T,
) -> T {
    let ctx = match callable {
        Some(obj) => CallContext::Obj(obj as *const crate::PyObject),
        None => CallContext::None,
    };
    let prev = CURRENT_CALL.with(|c| c.replace(ctx));
    let prev_offset = CURRENT_CALL_SELF_OFFSET.with(|c| c.replace(self_offset));
    let result = f();
    CURRENT_CALL_SELF_OFFSET.with(|c| c.set(prev_offset));
    CURRENT_CALL.with(|c| c.set(prev));
    result
}

pub fn hash_iter<'a, I: IntoIterator<Item = &'a PyObjectRef>>(
    iter: I,
    vm: &VirtualMachine,
) -> PyResult<rustpython_common::hash::PyHash> {
    vm.state.hash_secret.hash_iter(iter, |obj| obj.hash(vm))
}

impl ToPyObject for core::convert::Infallible {
    fn to_pyobject(self, _vm: &VirtualMachine) -> PyObjectRef {
        match self {}
    }
}

pub trait ToCString: AsRef<Wtf8> {
    fn to_cstring(&self, vm: &VirtualMachine) -> PyResult<alloc::ffi::CString> {
        alloc::ffi::CString::new(self.as_ref().as_bytes()).map_err(|err| err.to_pyexception(vm))
    }
}

impl ToCString for &str {}
impl ToCString for PyStr {}
impl ToCString for PyUtf8Str {}

pub(crate) fn collection_repr<'a, I>(
    class_name: Option<&str>,
    prefix: &str,
    suffix: &str,
    empty: &str,
    iter: I,
    vm: &VirtualMachine,
) -> PyResult<Wtf8Buf>
where
    I: core::iter::Iterator<Item = &'a PyObjectRef>,
{
    let mut repr = Wtf8Buf::new();
    if let Some(name) = class_name {
        repr.push_str(name);
        repr.push_char('(');
    }
    repr.push_str(prefix);
    {
        let mut parts_iter = iter.map(|o| o.repr(vm));
        let Some(first) = parts_iter.next().transpose()? else {
            return Ok(Wtf8Buf::from(empty));
        };
        repr.push_wtf8(first.as_wtf8());
        for part in parts_iter {
            repr.push_str(", ");
            repr.push_wtf8(part?.as_wtf8());
        }
    }
    repr.push_str(suffix);
    if class_name.is_some() {
        repr.push_char(')');
    }

    Ok(repr)
}

/// CPython's "Did you mean" keyword-argument suggestion (Objects/call.c).
///
/// Levenshtein distance with custom costs: substitution = 1, case-only
/// change = 0.5, insertion/deletion = 2. The candidate with the lowest cost
/// is suggested when its cost is `<= ELIMINATION_COST + ADDITION_COST` (4);
/// ties are broken toward the first candidate, matching CPython's
/// `dist < best_distance` scan.
pub fn suggest_keyword<'a>(
    name: &str,
    candidates: impl Iterator<Item = &'a str>,
) -> Option<&'a str> {
    const SUBSTITUTION_COST: f32 = 1.0;
    const ADDITION_COST: f32 = 2.0;
    const ELIMINATION_COST: f32 = 2.0;
    const CASE_COST: f32 = 0.5;
    // CPython suggests when the best distance is within the combined
    // insertion/deletion budget (Objects/call.c): e.g. "bluch"->"bluchin"
    // (two insertions, cost 4) is suggested, so the bound is 4, not 2.
    const MAX_SUGGESTION_COST: f32 = ELIMINATION_COST + ADDITION_COST;

    let a: Vec<char> = name.chars().collect();
    let mut best: Option<(&'a str, f32)> = None;
    for candidate in candidates {
        let b: Vec<char> = candidate.chars().collect();
        let dist = levenshtein_costs(
            &a,
            &b,
            SUBSTITUTION_COST,
            ELIMINATION_COST,
            ADDITION_COST,
            CASE_COST,
        );
        if dist <= MAX_SUGGESTION_COST && best.is_none_or(|(_, best_dist)| dist < best_dist) {
            best = Some((candidate, dist));
        }
    }
    best.map(|(candidate, _)| candidate)
}

fn levenshtein_costs(
    a: &[char],
    b: &[char],
    substitution: f32,
    elimination: f32,
    addition: f32,
    case: f32,
) -> f32 {
    // Standard Levenshtein with per-op costs. Insertion and deletion are both
    // ELIMINATION/ADDITION_COST (identical in CPython), so orientation is
    // irrelevant; only substitution (1.0) and case-change (0.5) differ.
    let mut prev: Vec<f32> = (0..=b.len()).map(|j| j as f32 * addition).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur: Vec<f32> = vec![0.0; b.len() + 1];
        cur[0] = (i as f32 + 1.0) * elimination;
        for (j, cb) in b.iter().enumerate() {
            let sub_cost = if ca == cb {
                0.0
            } else if ca.eq_ignore_ascii_case(cb) {
                case
            } else {
                substitution
            };
            cur[j + 1] = (prev[j] + sub_cost)
                .min(cur[j] + addition)
                .min(prev[j + 1] + elimination);
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::suggest_keyword;

    fn suggest(name: &str, candidates: &[&str]) -> Option<String> {
        suggest_keyword(name, candidates.iter().copied()).map(str::to_owned)
    }

    // Cases from CPython's test_call.TestErrorMessagesSuggestions.
    #[test]
    fn suggestion_substitution() {
        assert_eq!(suggest("bluch", &["noise", "more_noise", "a", "blech"]), Some("blech".into()));
    }

    #[test]
    fn suggestion_elimination() {
        assert_eq!(suggest("bluch", &["noise", "more_noise", "a", "blch"]), Some("blch".into()));
    }

    #[test]
    fn suggestion_addition() {
        // Two insertions (cost 4) is still within the suggestion bound.
        assert_eq!(
            suggest("bluch", &["noise", "more_noise", "a", "bluchin"]),
            Some("bluchin".into())
        );
    }

    #[test]
    fn suggestion_case_change_beats_substitution() {
        assert_eq!(suggest("bluch", &["BLuch", "Luch", "fluch"]), Some("BLuch".into()));
    }

    #[test]
    fn suggestion_tie_breaks_to_first() {
        assert_eq!(suggest("bluch", &["bluc", "blucha"]), Some("bluc".into()));
        assert_eq!(suggest("bluch", &["blach", "bluc"]), Some("blach".into()));
    }

    #[test]
    fn suggestion_too_far_is_none() {
        assert_eq!(suggest("orgs", &["aaa", "late1"]), None);
        assert_eq!(suggest("blach", &["aaa", "late1"]), None);
        assert_eq!(suggest("blech", &["sep", "maxsplit"]), None);
    }

    #[test]
    fn suggestion_insertion_deletion() {
        assert_eq!(suggest("aa", &["aaa"]), Some("aaa".into()));
        assert_eq!(suggest("late11", &["late1"]), Some("late1".into()));
        assert_eq!(suggest("maxsplt", &["sep", "maxsplit"]), Some("maxsplit".into()));
        assert_eq!(suggest("namez", &["name", "path"]), Some("name".into()));
    }
}
