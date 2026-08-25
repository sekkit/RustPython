pub(crate) use _json::module_def;
mod machinery;

#[pymodule]
mod _json {
    use super::machinery;
    use crate::vm::{
        AsObject, Py, PyObjectRef, PyPayload, PyResult, VirtualMachine,
        builtins::{PyBaseExceptionRef, PyDict, PyFloat, PyInt, PyList, PyStr, PyStrRef, PyTuple, PyType},
        convert::ToPyResult,
        function::{FuncArgs, IntoFuncArgs, OptionalArg},
        protocol::PyIterReturn,
        types::{Callable, Constructor},
    };
    use core::str::FromStr;
    use malachite_bigint::BigInt;
    use rustpython_common::wtf8::{Wtf8, Wtf8Buf};
    use std::collections::HashMap;

    /// Skip JSON whitespace characters (space, tab, newline, carriage return).
    /// Works with a byte slice and returns the number of bytes skipped.
    /// Since all JSON whitespace chars are ASCII, bytes == chars.
    #[inline]
    fn skip_whitespace(bytes: &[u8]) -> usize {
        flame_guard!("_json::skip_whitespace");
        let mut count = 0;
        for &b in bytes {
            match b {
                b' ' | b'\t' | b'\n' | b'\r' => count += 1,
                _ => break,
            }
        }
        count
    }

    /// Check if a byte slice starts with a given ASCII pattern.
    #[inline]
    fn starts_with_bytes(bytes: &[u8], pattern: &[u8]) -> bool {
        bytes.len() >= pattern.len() && &bytes[..pattern.len()] == pattern
    }

    #[pyattr(name = "make_scanner")]
    #[pyclass(name = "Scanner", traverse)]
    #[derive(Debug, PyPayload)]
    struct JsonScanner {
        #[pytraverse(skip)]
        strict: bool,
        object_hook: Option<PyObjectRef>,
        object_pairs_hook: Option<PyObjectRef>,
        parse_float: Option<PyObjectRef>,
        parse_int: Option<PyObjectRef>,
        parse_constant: PyObjectRef,
        ctx: PyObjectRef,
    }

    impl Constructor for JsonScanner {
        type Args = PyObjectRef;

        fn py_new(_cls: &Py<PyType>, ctx: Self::Args, vm: &VirtualMachine) -> PyResult<Self> {
            let strict = ctx.get_attr("strict", vm)?.try_to_bool(vm)?;
            let object_hook = vm.option_if_none(ctx.get_attr("object_hook", vm)?);
            let object_pairs_hook = vm.option_if_none(ctx.get_attr("object_pairs_hook", vm)?);
            let parse_float = ctx.get_attr("parse_float", vm)?;
            let parse_float = if vm.is_none(&parse_float) || parse_float.is(vm.ctx.types.float_type)
            {
                None
            } else {
                Some(parse_float)
            };
            let parse_int = ctx.get_attr("parse_int", vm)?;
            let parse_int = if vm.is_none(&parse_int) || parse_int.is(vm.ctx.types.int_type) {
                None
            } else {
                Some(parse_int)
            };
            let parse_constant = ctx.get_attr("parse_constant", vm)?;

            Ok(Self {
                strict,
                object_hook,
                object_pairs_hook,
                parse_float,
                parse_int,
                parse_constant,
                ctx,
            })
        }
    }

    #[pyclass(with(Callable, Constructor))]
    impl JsonScanner {
        fn parse(
            &self,
            pystr: PyStrRef,
            char_idx: usize,
            byte_idx: usize,
            scan_once: PyObjectRef,
            vm: &VirtualMachine,
        ) -> PyResult<PyIterReturn> {
            flame_guard!("JsonScanner::parse");
            let bytes = pystr.as_bytes();
            let wtf8 = pystr.as_wtf8();

            let first_byte = match bytes.get(byte_idx) {
                Some(&b) => b,
                None => {
                    return Ok(PyIterReturn::StopIteration(Some(
                        vm.ctx.new_int(char_idx).into(),
                    )));
                }
            };

            match first_byte {
                b'"' => {
                    // Parse string - pass slice starting after the quote
                    let (wtf8_result, chars_consumed, _bytes_consumed) =
                        machinery::scanstring(&wtf8[byte_idx + 1..], char_idx + 1, self.strict)
                            .map_err(|e| py_decode_error(e, pystr.clone(), vm))?;
                    let end_char_idx = char_idx + 1 + chars_consumed;
                    return Ok(PyIterReturn::Return(
                        vm.new_tuple((wtf8_result, end_char_idx)).into(),
                    ));
                }
                b'{' => {
                    // Parse object in Rust
                    let mut memo = HashMap::new();
                    return self
                        .parse_object(pystr, char_idx + 1, byte_idx + 1, &scan_once, &mut memo, vm)
                        .map(|(obj, end_char, _end_byte)| {
                            PyIterReturn::Return(vm.new_tuple((obj, end_char)).into())
                        });
                }
                b'[' => {
                    // Parse array in Rust
                    let mut memo = HashMap::new();
                    return self
                        .parse_array(pystr, char_idx + 1, byte_idx + 1, &scan_once, &mut memo, vm)
                        .map(|(obj, end_char, _end_byte)| {
                            PyIterReturn::Return(vm.new_tuple((obj, end_char)).into())
                        });
                }
                _ => {}
            }

            let rest = &bytes[byte_idx..];

            macro_rules! parse_const {
                ($s:literal, $val:expr) => {
                    if rest.starts_with($s.as_bytes()) {
                        return Ok(PyIterReturn::Return(
                            vm.new_tuple(($val, char_idx + $s.len())).into(),
                        ));
                    }
                };
            }

            parse_const!("null", vm.ctx.none());
            parse_const!("true", true);
            parse_const!("false", false);

            if let Some((res, len)) = self.parse_number(rest, vm) {
                return Ok(PyIterReturn::Return(
                    vm.new_tuple((res?, char_idx + len)).into(),
                ));
            }

            macro_rules! parse_constant {
                ($s:literal) => {
                    if rest.starts_with($s.as_bytes()) {
                        return Ok(PyIterReturn::Return(
                            vm.new_tuple((
                                self.parse_constant.call(($s,), vm)?,
                                char_idx + $s.len(),
                            ))
                            .into(),
                        ));
                    }
                };
            }

            parse_constant!("NaN");
            parse_constant!("Infinity");
            parse_constant!("-Infinity");

            Ok(PyIterReturn::StopIteration(Some(
                vm.ctx.new_int(char_idx).into(),
            )))
        }

        fn parse_number(&self, bytes: &[u8], vm: &VirtualMachine) -> Option<(PyResult, usize)> {
            flame_guard!("JsonScanner::parse_number");
            // RFC 8259 defines JSON numbers in ASCII syntax, including digits,
            // '-', '.', 'e'/'E', and an optional exponent sign, so byte iteration
            // is equivalent to char iteration here.
            let mut i = 0;
            if bytes.get(i) == Some(&b'-') {
                i += 1;
            }
            match bytes.get(i) {
                Some(b'0') => i += 1,
                Some(b'1'..=b'9') => {
                    i += 1;
                    while matches!(bytes.get(i), Some(b'0'..=b'9')) {
                        i += 1;
                    }
                }
                _ => return None,
            }

            let mut is_float = false;
            if bytes.get(i) == Some(&b'.') && matches!(bytes.get(i + 1), Some(b'0'..=b'9')) {
                is_float = true;
                i += 2;
                while matches!(bytes.get(i), Some(b'0'..=b'9')) {
                    i += 1;
                }
            }

            if matches!(bytes.get(i), Some(b'e' | b'E')) {
                let mut exponent_end = i + 1;
                if matches!(bytes.get(exponent_end), Some(b'+' | b'-')) {
                    exponent_end += 1;
                }
                if matches!(bytes.get(exponent_end), Some(b'0'..=b'9')) {
                    is_float = true;
                    exponent_end += 1;
                    while matches!(bytes.get(exponent_end), Some(b'0'..=b'9')) {
                        exponent_end += 1;
                    }
                    i = exponent_end;
                }
            }

            // SAFETY: the loop above accepts only ASCII bytes, so bytes[..i] is valid UTF-8.
            let buf = unsafe { core::str::from_utf8_unchecked(&bytes[..i]) };
            let ret = if is_float {
                // float
                if let Some(ref parse_float) = self.parse_float {
                    parse_float.call((buf,), vm)
                } else {
                    Ok(vm.ctx.new_float(f64::from_str(buf).unwrap()).into())
                }
            } else if let Some(ref parse_int) = self.parse_int {
                parse_int.call((buf,), vm)
            } else {
                Ok(vm.new_pyobj(BigInt::from_str(buf).unwrap()))
            };
            Some((ret, buf.len()))
        }

        /// Parse a JSON object starting after the opening '{'.
        /// Returns (parsed_object, end_char_index, end_byte_index).
        fn parse_object(
            &self,
            pystr: PyStrRef,
            start_char_idx: usize,
            start_byte_idx: usize,
            scan_once: &PyObjectRef,
            memo: &mut HashMap<Wtf8Buf, PyStrRef>,
            vm: &VirtualMachine,
        ) -> PyResult<(PyObjectRef, usize, usize)> {
            flame_guard!("JsonScanner::parse_object");

            let bytes = pystr.as_bytes();
            let wtf8 = pystr.as_wtf8();
            let mut char_idx = start_char_idx;
            let mut byte_idx = start_byte_idx;

            // Skip initial whitespace
            let ws = skip_whitespace(&bytes[byte_idx..]);
            char_idx += ws;
            byte_idx += ws;

            // Check for empty object
            match bytes.get(byte_idx) {
                Some(b'}') => {
                    return self.finalize_object(vec![], char_idx + 1, byte_idx + 1, vm);
                }
                Some(b'"') => {
                    // Continue to parse first key
                }
                _ => {
                    return Err(self.make_decode_error(
                        "Expecting property name enclosed in double quotes",
                        pystr,
                        char_idx,
                        vm,
                    ));
                }
            }

            let mut pairs: Vec<(PyObjectRef, PyObjectRef)> = Vec::new();

            loop {
                // We're now at '"', skip it
                char_idx += 1;
                byte_idx += 1;

                // Parse key string using scanstring with byte slice
                let (key_wtf8, chars_consumed, bytes_consumed) =
                    machinery::scanstring(&wtf8[byte_idx..], char_idx, self.strict)
                        .map_err(|e| py_decode_error(e, pystr.clone(), vm))?;

                char_idx += chars_consumed;
                byte_idx += bytes_consumed;

                // Key memoization - reuse existing key strings.
                // Keyed by Wtf8Buf so lone surrogates in keys (legal per Python str)
                // are preserved; using String here would lossy-collapse surrogates to U+FFFD.
                let key: PyObjectRef = match memo.get(&key_wtf8) {
                    Some(cached) => cached.clone().into(),
                    None => {
                        let py_key = vm.ctx.new_str(key_wtf8.clone());
                        memo.insert(key_wtf8, py_key.clone());
                        py_key.into()
                    }
                };

                // Skip whitespace after key
                let ws = skip_whitespace(&bytes[byte_idx..]);
                char_idx += ws;
                byte_idx += ws;

                // Expect ':' delimiter
                match bytes.get(byte_idx) {
                    Some(b':') => {
                        char_idx += 1;
                        byte_idx += 1;
                    }
                    _ => {
                        return Err(self.make_decode_error(
                            "Expecting ':' delimiter",
                            pystr,
                            char_idx,
                            vm,
                        ));
                    }
                }

                // Skip whitespace after ':'
                let ws = skip_whitespace(&bytes[byte_idx..]);
                char_idx += ws;
                byte_idx += ws;

                // Parse value recursively
                let (value, value_char_end, value_byte_end) =
                    self.call_scan_once(scan_once, pystr.clone(), char_idx, byte_idx, memo, vm)?;

                pairs.push((key, value));
                char_idx = value_char_end;
                byte_idx = value_byte_end;

                // Skip whitespace after value
                let ws = skip_whitespace(&bytes[byte_idx..]);
                char_idx += ws;
                byte_idx += ws;

                // Check for ',' or '}'
                match bytes.get(byte_idx) {
                    Some(b'}') => {
                        char_idx += 1;
                        byte_idx += 1;
                        break;
                    }
                    Some(b',') => {
                        let comma_char_idx = char_idx;
                        char_idx += 1;
                        byte_idx += 1;

                        // Skip whitespace after comma
                        let ws = skip_whitespace(&bytes[byte_idx..]);
                        char_idx += ws;
                        byte_idx += ws;

                        // Next must be '"'
                        match bytes.get(byte_idx) {
                            Some(b'"') => {
                                // Continue to next key-value pair
                            }
                            Some(b'}') => {
                                // Trailing comma before end of object
                                return Err(self.make_decode_error(
                                    "Illegal trailing comma before end of object",
                                    pystr,
                                    comma_char_idx,
                                    vm,
                                ));
                            }
                            _ => {
                                return Err(self.make_decode_error(
                                    "Expecting property name enclosed in double quotes",
                                    pystr,
                                    char_idx,
                                    vm,
                                ));
                            }
                        }
                    }
                    _ => {
                        return Err(self.make_decode_error(
                            "Expecting ',' delimiter",
                            pystr,
                            char_idx,
                            vm,
                        ));
                    }
                }
            }

            self.finalize_object(pairs, char_idx, byte_idx, vm)
        }

        /// Parse a JSON array starting after the opening '['.
        /// Returns (parsed_array, end_char_index, end_byte_index).
        fn parse_array(
            &self,
            pystr: PyStrRef,
            start_char_idx: usize,
            start_byte_idx: usize,
            scan_once: &PyObjectRef,
            memo: &mut HashMap<Wtf8Buf, PyStrRef>,
            vm: &VirtualMachine,
        ) -> PyResult<(PyObjectRef, usize, usize)> {
            flame_guard!("JsonScanner::parse_array");

            let bytes = pystr.as_bytes();
            let mut char_idx = start_char_idx;
            let mut byte_idx = start_byte_idx;

            // Skip initial whitespace
            let ws = skip_whitespace(&bytes[byte_idx..]);
            char_idx += ws;
            byte_idx += ws;

            // Check for empty array
            if bytes.get(byte_idx) == Some(&b']') {
                return Ok((vm.ctx.new_list(vec![]).into(), char_idx + 1, byte_idx + 1));
            }

            let mut values: Vec<PyObjectRef> = Vec::new();

            loop {
                // Parse value
                let (value, value_char_end, value_byte_end) =
                    self.call_scan_once(scan_once, pystr.clone(), char_idx, byte_idx, memo, vm)?;

                values.push(value);
                char_idx = value_char_end;
                byte_idx = value_byte_end;

                // Skip whitespace after value
                let ws = skip_whitespace(&bytes[byte_idx..]);
                char_idx += ws;
                byte_idx += ws;

                match bytes.get(byte_idx) {
                    Some(b']') => {
                        char_idx += 1;
                        byte_idx += 1;
                        break;
                    }
                    Some(b',') => {
                        let comma_char_idx = char_idx;
                        char_idx += 1;
                        byte_idx += 1;

                        // Skip whitespace after comma
                        let ws = skip_whitespace(&bytes[byte_idx..]);
                        char_idx += ws;
                        byte_idx += ws;

                        // Check for trailing comma
                        if bytes.get(byte_idx) == Some(&b']') {
                            return Err(self.make_decode_error(
                                "Illegal trailing comma before end of array",
                                pystr,
                                comma_char_idx,
                                vm,
                            ));
                        }
                    }
                    _ => {
                        return Err(self.make_decode_error(
                            "Expecting ',' delimiter",
                            pystr,
                            char_idx,
                            vm,
                        ));
                    }
                }
            }

            Ok((vm.ctx.new_list(values).into(), char_idx, byte_idx))
        }

        /// Finalize object construction with hooks.
        fn finalize_object(
            &self,
            pairs: Vec<(PyObjectRef, PyObjectRef)>,
            end_char_idx: usize,
            end_byte_idx: usize,
            vm: &VirtualMachine,
        ) -> PyResult<(PyObjectRef, usize, usize)> {
            let result = if let Some(ref pairs_hook) = self.object_pairs_hook {
                // object_pairs_hook takes priority - pass list of tuples
                let pairs_list: Vec<PyObjectRef> = pairs
                    .into_iter()
                    .map(|(k, v)| vm.new_tuple((k, v)).into())
                    .collect();
                pairs_hook.call((vm.ctx.new_list(pairs_list),), vm)?
            } else {
                // Build a dict from pairs
                let dict = vm.ctx.new_dict();
                for (key, value) in pairs {
                    dict.set_item(&*key, value, vm)?;
                }

                // Apply object_hook if present
                let dict_obj: PyObjectRef = dict.into();
                if let Some(ref hook) = self.object_hook {
                    hook.call((dict_obj,), vm)?
                } else {
                    dict_obj
                }
            };

            Ok((result, end_char_idx, end_byte_idx))
        }

        /// Call scan_once and handle the result.
        /// Returns (value, end_char_idx, end_byte_idx).
        fn call_scan_once(
            &self,
            scan_once: &PyObjectRef,
            pystr: PyStrRef,
            char_idx: usize,
            byte_idx: usize,
            memo: &mut HashMap<Wtf8Buf, PyStrRef>,
            vm: &VirtualMachine,
        ) -> PyResult<(PyObjectRef, usize, usize)> {
            // Recursion guard: parse_object/parse_array recurse into call_scan_once
            // for each child value. Without this, a deeply-nested input like
            // `'[' * 50000 + ']' * 50000` overflows the native Rust stack and
            // crashes the process with SIGSEGV. Matches CPython's
            // _Py_EnterRecursiveCall in Modules/_json.c.
            vm.with_recursion("while decoding a JSON object from a string", || {
                let bytes = pystr.as_bytes();
                let wtf8 = pystr.as_wtf8();

                let first_byte = match bytes.get(byte_idx) {
                    Some(&b) => b,
                    None => {
                        return Err(self.make_decode_error("Expecting value", pystr, char_idx, vm));
                    }
                };

                match first_byte {
                    b'"' => {
                        // String - pass slice starting after the quote.
                        // Feed the Wtf8Buf directly to new_str; routing through
                        // .to_string() here would lossy-collapse surrogates to U+FFFD.
                        let (wtf8_result, chars_consumed, bytes_consumed) =
                            machinery::scanstring(&wtf8[byte_idx + 1..], char_idx + 1, self.strict)
                                .map_err(|e| py_decode_error(e, pystr.clone(), vm))?;
                        let py_str = vm.ctx.new_str(wtf8_result);
                        Ok((
                            py_str.into(),
                            char_idx + 1 + chars_consumed,
                            byte_idx + 1 + bytes_consumed,
                        ))
                    }
                    b'{' => {
                        // Object
                        self.parse_object(pystr, char_idx + 1, byte_idx + 1, scan_once, memo, vm)
                    }
                    b'[' => {
                        // Array
                        self.parse_array(pystr, char_idx + 1, byte_idx + 1, scan_once, memo, vm)
                    }
                    b'n' if starts_with_bytes(&bytes[byte_idx..], b"null") => {
                        // null
                        Ok((vm.ctx.none(), char_idx + 4, byte_idx + 4))
                    }
                    b't' if starts_with_bytes(&bytes[byte_idx..], b"true") => {
                        // true
                        Ok((vm.ctx.new_bool(true).into(), char_idx + 4, byte_idx + 4))
                    }
                    b'f' if starts_with_bytes(&bytes[byte_idx..], b"false") => {
                        // false
                        Ok((vm.ctx.new_bool(false).into(), char_idx + 5, byte_idx + 5))
                    }
                    b'N' if starts_with_bytes(&bytes[byte_idx..], b"NaN") => {
                        // NaN
                        let result = self.parse_constant.call(("NaN",), vm)?;
                        Ok((result, char_idx + 3, byte_idx + 3))
                    }
                    b'I' if starts_with_bytes(&bytes[byte_idx..], b"Infinity") => {
                        // Infinity
                        let result = self.parse_constant.call(("Infinity",), vm)?;
                        Ok((result, char_idx + 8, byte_idx + 8))
                    }
                    b'-' => {
                        // -Infinity or negative number
                        if starts_with_bytes(&bytes[byte_idx..], b"-Infinity") {
                            let result = self.parse_constant.call(("-Infinity",), vm)?;
                            return Ok((result, char_idx + 9, byte_idx + 9));
                        }
                        // Negative number - numbers are ASCII so len == bytes
                        if let Some((result, len)) = self.parse_number(&bytes[byte_idx..], vm) {
                            return Ok((result?, char_idx + len, byte_idx + len));
                        }
                        Err(self.make_decode_error("Expecting value", pystr, char_idx, vm))
                    }
                    b'0'..=b'9' => {
                        // Positive number - numbers are ASCII so len == bytes
                        if let Some((result, len)) = self.parse_number(&bytes[byte_idx..], vm) {
                            return Ok((result?, char_idx + len, byte_idx + len));
                        }
                        Err(self.make_decode_error("Expecting value", pystr, char_idx, vm))
                    }
                    _ => {
                        // Fall back to scan_once for unrecognized input
                        // Note: This path requires char_idx for Python compatibility
                        let result = scan_once.call((pystr.clone(), char_idx as isize), vm);

                        match result {
                            Ok(tuple) => {
                                use crate::vm::builtins::PyTupleRef;
                                let tuple: PyTupleRef = tuple.try_into_value(vm)?;
                                if tuple.len() != 2 {
                                    return Err(vm.new_value_error("scan_once must return 2-tuple"));
                                }
                                let value = tuple.as_slice()[0].clone();
                                let end_char_idx: isize = tuple.as_slice()[1].try_to_value(vm)?;
                                // For fallback, we need to calculate byte_idx from char_idx
                                // This is expensive but fallback should be rare
                                let end_byte_idx = wtf8
                                    .code_point_indices()
                                    .nth(end_char_idx as usize)
                                    .map_or(wtf8.len(), |(i, _)| i);
                                Ok((value, end_char_idx as usize, end_byte_idx))
                            }
                            Err(err) if err.fast_isinstance(vm.ctx.exceptions.stop_iteration) => {
                                Err(self.make_decode_error("Expecting value", pystr, char_idx, vm))
                            }
                            Err(err) => Err(err),
                        }
                    }
                }
            })
        }

        /// Create a decode error.
        fn make_decode_error(
            &self,
            msg: &str,
            s: PyStrRef,
            pos: usize,
            vm: &VirtualMachine,
        ) -> PyBaseExceptionRef {
            let err = machinery::DecodeError::new(msg, pos);
            py_decode_error(err, s, vm)
        }
    }

    impl Callable for JsonScanner {
        type Args = (PyStrRef, isize);
        fn call(zelf: &Py<Self>, (pystr, char_idx): Self::Args, vm: &VirtualMachine) -> PyResult {
            if char_idx < 0 {
                return Err(vm.new_value_error("idx cannot be negative"));
            }
            let char_idx = char_idx as usize;
            let wtf8 = pystr.as_wtf8();

            // Calculate byte index from char index (O(char_idx) but only at entry point)
            let byte_idx = if char_idx == 0 {
                0
            } else {
                match wtf8.code_point_indices().nth(char_idx) {
                    Some((byte_i, _)) => byte_i,
                    None => {
                        // char_idx is beyond the string length
                        return PyIterReturn::StopIteration(Some(vm.ctx.new_int(char_idx).into()))
                            .to_pyresult(vm);
                    }
                }
            };

            zelf.parse(pystr, char_idx, byte_idx, zelf.to_owned().into(), vm)
                .and_then(|x| x.to_pyresult(vm))
        }
    }

    fn encode_string(wtf8: &rustpython_common::wtf8::Wtf8, ascii_only: bool) -> Wtf8Buf {
        flame_guard!("_json::encode_string");
        let mut buf = Vec::<u8>::with_capacity(wtf8.len() + 2);
        machinery::write_json_string(wtf8, ascii_only, &mut buf)
            // SAFETY: writing to a vec can't fail
            .unwrap_or_else(|_| unsafe { core::hint::unreachable_unchecked() });
        // write_json_string is designed to produce valid WTF-8 bytes:
        // - ASCII control characters and JSON-specials are written as ASCII escapes
        // - Valid Unicode scalars are written as UTF-8 (a subset of WTF-8)
        // - Lone surrogates (ascii_only=false branch only) pass through as the
        //   input's WTF-8 byte sequences unchanged
        // Use the checked constructor so any violation of that invariant
        // surfaces as a panic during testing instead of undefined behavior.
        Wtf8Buf::from_bytes(buf).expect("write_json_string produced invalid WTF-8")
    }

    #[pyfunction]
    fn encode_basestring(s: PyStrRef) -> Wtf8Buf {
        encode_string(s.as_wtf8(), false)
    }

    #[pyfunction]
    fn encode_basestring_ascii(s: PyStrRef) -> Wtf8Buf {
        encode_string(s.as_wtf8(), true)
    }

    fn py_decode_error(
        e: machinery::DecodeError,
        s: PyStrRef,
        vm: &VirtualMachine,
    ) -> PyBaseExceptionRef {
        let get_error = || -> PyResult<_> {
            let cls = vm.try_class("json", "JSONDecodeError")?;
            let exc = PyType::call(&cls, (e.msg, s, e.pos).into_args(vm), vm)?;
            exc.try_into_value(vm)
        };
        match get_error() {
            Ok(x) | Err(x) => x,
        }
    }

    #[pyfunction]
    fn scanstring(
        s: PyStrRef,
        end: usize,
        strict: OptionalArg<bool>,
        vm: &VirtualMachine,
    ) -> PyResult<(Wtf8Buf, usize)> {
        flame_guard!("_json::scanstring");
        let wtf8 = s.as_wtf8();

        // Convert char index `end` to byte index
        let byte_idx = if end == 0 {
            0
        } else {
            wtf8.code_point_indices()
                .nth(end)
                .map(|(i, _)| i)
                .ok_or_else(|| {
                    py_decode_error(
                        machinery::DecodeError::new("Unterminated string starting at", end - 1),
                        s.clone(),
                        vm,
                    )
                })?
        };

        let (result, chars_consumed, _bytes_consumed) =
            machinery::scanstring(&wtf8[byte_idx..], end, strict.unwrap_or(true))
                .map_err(|e| py_decode_error(e, s, vm))?;

        Ok((result, end + chars_consumed))
    }

    // ---- Native JSON encoder (_json.make_encoder) ----

    /// Recursively serialize a Python object tree to JSON WTF-8 bytes.
    fn encode_json_value(
        obj: &PyObjectRef,
        seen: &mut Vec<usize>,
        default_fn: Option<&PyObjectRef>,
        encoder_fn: Option<&PyObjectRef>,
        allow_nan: bool,
        ascii_only: bool,
        check_circular: bool,
        key_sep: &Wtf8,
        item_sep: &Wtf8,
        indent: &Option<Wtf8Buf>,
        sort_keys: bool,
        skip_keys: bool,
        level: usize,
        vm: &VirtualMachine,
    ) -> PyResult<Wtf8Buf> {
        if vm.is_none(obj) {
            return Ok(Wtf8Buf::from("null"));
        }
        if obj.is(&vm.ctx.true_value) {
            return Ok(Wtf8Buf::from("true"));
        }
        if obj.is(&vm.ctx.false_value) {
            return Ok(Wtf8Buf::from("false"));
        }
        if let Some(s) = obj.downcast_ref_if_exact::<PyStr>(vm) {
            if let Some(enc) = encoder_fn {
                let encoded = enc.as_object().call((obj.to_owned(),), vm)?;
                let estr = encoded.downcast_ref::<PyStr>().ok_or_else(|| {
                    vm.new_type_error("encoder must return a str")
                })?;
                return Ok(encode_string(estr.as_wtf8(), false));
            }
            return Ok(encode_string(s.as_wtf8(), ascii_only));
        }
        if let Some(i) = obj.downcast_ref::<PyInt>() {
            return Ok(Wtf8Buf::from(format!("{}", i.as_bigint())));
        }
        if let Some(fl) = obj.downcast_ref::<PyFloat>() {
            let v = fl.to_f64();
            if v.is_nan() {
                if !allow_nan {
                    return Err(vm.new_value_error(
                        "Out of range float values are not JSON compliant: nan",
                    ));
                }
                return Ok(Wtf8Buf::from("NaN"));
            }
            if v.is_infinite() {
                if !allow_nan {
                    return Err(vm.new_value_error(
                        format!("Out of range float values are not JSON compliant: {v}"),
                    ));
                }
                return Ok(Wtf8Buf::from(if v > 0.0 { "Infinity" } else { "-Infinity" }));
            }
            return Ok(Wtf8Buf::from(rustpython_vm::literal::float::to_string(v)));
        }
        // Non-scalar: circular reference detection covers every object that
        // is not an immutable JSON scalar (matches CPython's marker set).
        // When check_circular is disabled the recursion depth is bounded by
        // the interpreter's own stack (RecursionError), like CPython.
        if check_circular {
            let ptr_id = (obj.as_object().as_raw() as *const u8) as usize;
            if seen.contains(&ptr_id) {
                return Err(vm.new_value_error("Circular reference detected"));
            }
            seen.push(ptr_id);
            let result = encode_json_container(
                obj, ptr_id, seen, default_fn, encoder_fn, allow_nan, ascii_only, check_circular, key_sep, item_sep,
                indent, sort_keys, skip_keys, level, vm,
            );
            seen.pop();
            result
        } else {
            let ptr_id = (obj.as_object().as_raw() as *const u8) as usize;
            encode_json_container(
                obj, ptr_id, seen, default_fn, encoder_fn, allow_nan, ascii_only, check_circular, key_sep, item_sep,
                indent, sort_keys, skip_keys, level, vm,
            )
        }
    }

    /// Encode a non-scalar object (list / tuple / dict / default-converted).
    /// Runs with the object already registered in `seen`.
    fn encode_json_container(
        obj: &PyObjectRef,
        ptr_id: usize,
        seen: &mut Vec<usize>,
        default_fn: Option<&PyObjectRef>,
        encoder_fn: Option<&PyObjectRef>,
        allow_nan: bool,
        ascii_only: bool,
        check_circular: bool,
        key_sep: &Wtf8,
        item_sep: &Wtf8,
        indent: &Option<Wtf8Buf>,
        sort_keys: bool,
        skip_keys: bool,
        level: usize,
        vm: &VirtualMachine,
    ) -> PyResult<Wtf8Buf> {
        // list — iterate the LIVE list by index so default() callbacks that
        // mutate the list during encoding produce CPython's observed output.
        if let Some(list) = obj.downcast_ref_if_exact::<PyList>(vm) {
            let mut buf = Wtf8Buf::from("[");
            let mut i = 0usize;
            let mut n = 0usize;
            loop {
                let item = {
                    let guard = list.borrow_vec();
                    if i >= guard.len() {
                        break;
                    }
                    guard[i].clone()
                };
                if n > 0 {
                    buf.push_wtf8(item_sep);
                }
                buf.push_wtf8(&indent_prefix(indent, level + 1));
                let sub = encode_json_value(&item, seen, default_fn, encoder_fn, allow_nan, ascii_only, check_circular, key_sep, item_sep, indent, sort_keys, skip_keys, level + 1, vm);
                match sub {
                    Ok(text) => buf.push_wtf8(&text),
                    Err(e) => {
                        note_on_circular(&e, vm, format!("when serializing list item {n}"));
                        return Err(e);
                    }
                }
                i += 1;
                n += 1;
            }
            buf.push_wtf8(&indent_suffix(indent, n, level));
            buf.push_wtf8(Wtf8Buf::from("]").as_slice());
            return Ok(buf);
        }
        // tuple (serialized as array)
        if let Some(tuple) = obj.downcast_ref_if_exact::<PyTuple>(vm) {
            let elements = tuple.as_slice();
            let mut buf = Wtf8Buf::from("[");
            for (n, item) in elements.iter().enumerate() {
                if n > 0 {
                    buf.push_wtf8(item_sep);
                }
                buf.push_wtf8(&indent_prefix(indent, level + 1));
                let sub = encode_json_value(item, seen, default_fn, encoder_fn, allow_nan, ascii_only, check_circular, key_sep, item_sep, indent, sort_keys, skip_keys, level + 1, vm);
                match sub {
                    Ok(text) => buf.push_wtf8(&text),
                    Err(e) => {
                        note_on_circular(&e, vm, format!("when serializing tuple item {n}"));
                        return Err(e);
                    }
                }
            }
            buf.push_wtf8(&indent_suffix(indent, elements.len(), level));
            buf.push_wtf8(Wtf8Buf::from("]").as_slice());
            return Ok(buf);
        }
        // dict — exact dicts iterate the C-level entries (insertion order);
        // subclasses (OrderedDict etc.) iterate via the Python-level items()
        // protocol so custom ordering is respected.
        if let Some(dict) = obj.downcast_ref::<PyDict>() {
            let mut items = if obj.class().is(vm.ctx.types.dict_type) {
                dict.items_vec()
            } else {
                let items_obj = obj.as_object().get_attr("items", vm)?.call((), vm)?;
                let mut out = Vec::new();
                let mut iter = items_obj.as_object().get_iter(vm)?;
                loop {
                    match iter.next(vm)? {
                        crate::vm::protocol::PyIterReturn::Return(item) => {
                            let tup = item.downcast_ref::<crate::vm::builtins::PyTuple>().ok_or_else(|| {
                                vm.new_type_error("dict items() must yield 2-tuples")
                            })?;
                            let sl = tup.as_slice();
                            if sl.len() != 2 {
                                return Err(vm.new_type_error("dict items() must yield 2-tuples"));
                            }
                            out.push((sl[0].clone(), sl[1].clone()));
                        }
                        crate::vm::protocol::PyIterReturn::StopIteration(_) => break,
                    }
                }
                out
            };
            if sort_keys {
                let mut sort_err: Option<PyBaseExceptionRef> = None;
                items.sort_by(|a, b| {
                    let less = rustpython_vm::types::PyComparisonOp::Lt;
                    let a_lt = a.0.as_object().rich_compare_bool(&b.0, less, vm);
                    let b_lt = b.0.as_object().rich_compare_bool(&a.0, less, vm);
                    match (a_lt, b_lt) {
                        (Ok(true), _) => core::cmp::Ordering::Less,
                        (_, Ok(true)) => core::cmp::Ordering::Greater,
                        (Err(e), _) => {
                            if sort_err.is_none() {
                                sort_err = Some(e);
                            }
                            core::cmp::Ordering::Equal
                        }
                        _ => core::cmp::Ordering::Equal,
                    }
                });
                if let Some(e) = sort_err {
                    return Err(e);
                }
            }
            let mut buf = Wtf8Buf::from("{");
            let mut done = 0usize; // emitted pairs (skipped keys don't count)
            for (k, v) in items.iter() {
                let key_text: Wtf8Buf = match k.downcast_ref::<PyStr>() {
                    Some(s) => {
                        if let Some(enc) = encoder_fn {
                            let encoded = enc.as_object().call((k.to_owned(),), vm)?;
                            let estr = encoded.downcast_ref::<PyStr>().ok_or_else(|| {
                                vm.new_type_error("encoder must return a str")
                            })?;
                            encode_string(estr.as_wtf8(), false)
                        } else {
                            encode_string(s.as_wtf8(), ascii_only)
                        }
                    }
                    None if skip_keys => continue,
                    None => match encode_key_scalar(k, vm) {
                        Some(text) => {
                            let mut quoted = Wtf8Buf::from("\"");
                            quoted.push_wtf8(&text);
                            quoted.push_wtf8(Wtf8Buf::from("\"").as_slice());
                            quoted
                        }
                        None => {
                            return Err(vm.new_type_error(
                                format!("keys must be str, int, float, bool or None, not {}", k.class().name())
                            ));
                        }
                    },
                };
                if done > 0 {
                    buf.push_wtf8(item_sep);
                }
                buf.push_wtf8(&indent_prefix(indent, level + 1));
                buf.push_wtf8(&key_text);
                buf.push_wtf8(key_sep);
                let sub = encode_json_value(v, seen, default_fn, encoder_fn, allow_nan, ascii_only, check_circular, key_sep, item_sep, indent, sort_keys, skip_keys, level + 1, vm);
                match sub {
                    Ok(text) => buf.push_wtf8(&text),
                    Err(e) => {
                        note_on_circular(&e, vm, format!("when serializing dict item '{}'", key_note_text(k, vm)));
                        return Err(e);
                    }
                }
                done += 1;
            }
            buf.push_wtf8(&indent_suffix(indent, done, level));
            buf.push_wtf8(Wtf8Buf::from("}").as_slice());
            return Ok(buf);
        }
        // Unknown type: call default handler
        if let Some(default_fn) = default_fn {
            let _ = ptr_id;
            let replacement = default_fn.as_object().call((obj.to_owned(),), vm)?;
            if check_circular && replacement.is(obj) {
                return Err(vm.new_value_error("Circular reference detected"));
            }
            let sub = encode_json_value(&replacement, seen, Some(default_fn), encoder_fn, allow_nan, ascii_only, check_circular, key_sep, item_sep, indent, sort_keys, skip_keys, level, vm);
            match sub {
                Ok(text) => return Ok(text),
                Err(e) => {
                    note_on_circular(&e, vm, "when serializing type object".to_string());
                    return Err(e);
                }
            }
        }
        Err(vm.new_type_error(format!(
            "Object of type {} is not JSON serializable",
            obj.class().name()
        )))
    }

    /// Attach a PEP 678 note describing the container context to an error
    /// raised while serializing a child element (matches CPython's encoder).
    fn note_on_circular(e: &PyBaseExceptionRef, vm: &VirtualMachine, note: String) {
        let _ = vm.call_method(
            e.as_object(),
            "add_note",
            (vm.ctx.new_str(note),),
        );
    }

    /// JSON-quoted display of a dict key for recursion notes.
    fn key_note_text(k: &PyObjectRef, vm: &VirtualMachine) -> String {
        if let Some(s) = k.downcast_ref::<PyStr>() {
            s.as_wtf8().to_string()
        } else {
            format!("{}", k.repr(vm).as_ref().map(|r| r.as_wtf8().to_string()).unwrap_or_default())
        }
    }

    /// Encode non-str dict keys that are JSON scalars (bool/int/float/None)
    /// to their JSON text form ("true", "2", "4.0", "null"). Returns None
    /// for unsupported key types.
    fn encode_key_scalar(key: &PyObjectRef, vm: &VirtualMachine) -> Option<Wtf8Buf> {
        if key.is(&vm.ctx.true_value) {
            return Some(Wtf8Buf::from("true"));
        }
        if key.is(&vm.ctx.false_value) {
            return Some(Wtf8Buf::from("false"));
        }
        if vm.is_none(key) {
            return Some(Wtf8Buf::from("null"));
        }
        if let Some(i) = key.downcast_ref::<PyInt>() {
            return Some(Wtf8Buf::from(format!("{}", i.as_bigint())));
        }
        if let Some(f) = key.downcast_ref::<PyFloat>() {
            let v = f.to_f64();
            return Some(if v.is_nan() {
                Wtf8Buf::from("NaN")
            } else if v.is_infinite() {
                Wtf8Buf::from(if v > 0.0 { "Infinity" } else { "-Infinity" })
            } else {
                Wtf8Buf::from(rustpython_vm::literal::float::to_string(v))
            });
        }
        None
    }

    /// Indent prefix placed before each container item when `indent` is set.
    /// Returns "" for compact mode.
    fn indent_prefix(indent: &Option<Wtf8Buf>, level: usize) -> Wtf8Buf {
        match indent {
            Some(ind) => {
                let mut buf = Wtf8Buf::from("\n");
                for _ in 0..level {
                    buf.push_wtf8(ind.as_slice());
                }
                buf
            }
            None => Wtf8Buf::new(),
        }
    }

    /// Closing-brace indentation when `indent` is set (newline + level indents
    /// if the container has items; "" otherwise). Compact mode returns "".
    fn indent_suffix(indent: &Option<Wtf8Buf>, nitems: usize, level: usize) -> Wtf8Buf {
        match indent {
            Some(_) if nitems > 0 => {
                let mut buf = Wtf8Buf::from("\n");
                for _ in 0..level {
                    buf.push_wtf8(indent.as_ref().unwrap().as_slice());
                }
                buf
            }
            _ => Wtf8Buf::new(),
        }
    }

    #[pyclass(name = "_jsonEncoder", module = "_json", no_attr)]
    #[derive(Debug, PyPayload)]
    struct JsonEncoder {
        default_fn: Option<PyObjectRef>,
        encoder_fn: Option<PyObjectRef>,
        allow_nan: bool,
        ascii_only: bool,
        check_circular: bool,
        sort_keys: bool,
        skip_keys: bool,
        indent: Option<Wtf8Buf>,
        key_sep: Wtf8Buf,
        item_sep: Wtf8Buf,
    }

    #[pyclass(with(Callable))]
    impl JsonEncoder {}

    impl Callable for JsonEncoder {
        type Args = FuncArgs;

        fn call(zelf: &Py<Self>, mut args: FuncArgs, vm: &VirtualMachine) -> PyResult {
            let obj = args.args.remove(0);
            let _level: usize = args.args.get(1).and_then(|x| x.try_to_value(vm).ok()).unwrap_or(0);
            let mut seen = Vec::new();
            let result = encode_json_value(
                &obj, &mut seen, zelf.default_fn.as_ref(), zelf.encoder_fn.as_ref(), zelf.allow_nan, zelf.ascii_only, zelf.check_circular,
                &zelf.key_sep, &zelf.item_sep, &zelf.indent, zelf.sort_keys, zelf.skip_keys, 0, vm,
            )?;
            Ok(vm.ctx.new_str(result).into())
        }
    }

    #[pyfunction]
    fn make_encoder(args: FuncArgs, vm: &VirtualMachine) -> PyResult<PyObjectRef> {
        // markers must be a dict or None; None == check_circular disabled.
        let check_circular = match args.args.get(0) {
            Some(m) if vm.is_none(&m) => false,
            Some(m) if m.downcast_ref::<PyDict>().is_some() => true,
            _ => {
                return Err(vm.new_type_error("markers must be a dict or None"));
            }
        };
        let default_fn = args.args.get(1).cloned().filter(|x| !vm.is_none(x));
        // encoder_fn is encode_basestring_ascii (ensure_ascii=True, the
        // default) or encode_basestring; compare by __name__. The built-in
        // encoders are handled internally with `ascii_only`; any other
        // callable is stored and invoked for every string (keys and values).
        let (encoder_fn_arg, ascii_only) = match args.args.get(2).cloned().filter(|x| !vm.is_none(x)) {
            Some(f) => {
                let name = f.get_attr("__name__", vm).ok()
                    .and_then(|n| n.downcast_ref::<PyStr>().map(|s| s.as_wtf8().to_string()))
                    .unwrap_or_default();
                match name.as_str() {
                    "encode_basestring_ascii" => (None, true),
                    "encode_basestring" => (None, false),
                    _ => (Some(f), false), // custom encoder: call it for strings
                }
            }
            None => (None, true),
        };
        let indent = args.args.get(3).and_then(|x| {
            if vm.is_none(&x) { None }
            else { x.str(vm).ok().map(|s| s.as_wtf8().to_owned()) }
        });
        let key_sep = match args.args.get(4) {
            Some(x) if vm.is_none(&x) => Wtf8Buf::from(": "),
            Some(x) => match x.downcast_ref::<PyStr>() {
                Some(s) => s.as_wtf8().to_owned(),
                None => return Err(vm.new_type_error("key separator must be a string")),
            },
            None => Wtf8Buf::from(": "),
        };
        let item_sep = match args.args.get(5) {
            Some(x) if vm.is_none(&x) => Wtf8Buf::from(", "),
            Some(x) => match x.downcast_ref::<PyStr>() {
                Some(s) => s.as_wtf8().to_owned(),
                None => return Err(vm.new_type_error("item separator must be a string")),
            },
            None => Wtf8Buf::from(", "),
        };
        let sort_keys = args.args.get(6).map(|x| x.is(&vm.ctx.true_value)).unwrap_or(false);
        let skip_keys = args.args.get(7).map(|x| x.is(&vm.ctx.true_value)).unwrap_or(false);
        let allow_nan = args.args.get(8).map(|x| {
            if x.is(&vm.ctx.true_value) { true }
            else if x.is(&vm.ctx.false_value) { false }
            else { true }
        }).unwrap_or(true);
        let _ = ();
        let encoder = JsonEncoder { default_fn, encoder_fn: encoder_fn_arg, allow_nan, ascii_only, check_circular, sort_keys, skip_keys, indent, key_sep, item_sep };
        Ok(encoder.into_pyobject(vm))
    }
}
