use core::ops::{self, Range};

use num_traits::ToPrimitive;

use crate::str::StrKind;
use crate::wtf8::{CodePoint, Wtf8, Wtf8Buf};

pub trait StrBuffer: AsRef<Wtf8> {
    fn is_compatible_with(&self, kind: StrKind) -> bool {
        let s = self.as_ref();
        match kind {
            StrKind::Ascii => s.is_ascii(),
            StrKind::Utf8 => s.is_utf8(),
            StrKind::Wtf8 => true,
        }
    }
}

pub trait CodecContext: Sized {
    type Error;
    type StrBuf: StrBuffer;
    type BytesBuf: AsRef<[u8]>;

    fn string(&self, s: Wtf8Buf) -> Self::StrBuf;
    fn bytes(&self, b: Vec<u8>) -> Self::BytesBuf;
}

pub trait EncodeContext: CodecContext {
    fn full_data(&self) -> &Wtf8;
    fn data_len(&self) -> StrSize;

    fn remaining_data(&self) -> &Wtf8;
    fn position(&self) -> StrSize;

    fn restart_from(&mut self, pos: StrSize) -> Result<(), Self::Error>;

    fn error_encoding(&self, range: Range<StrSize>, reason: Option<&str>) -> Self::Error;

    fn handle_error<E>(
        &mut self,
        errors: &E,
        range: Range<StrSize>,
        reason: Option<&str>,
    ) -> Result<EncodeReplace<Self>, Self::Error>
    where
        E: EncodeErrorHandler<Self>,
    {
        let (replace, restart) = errors.handle_encode_error(self, range, reason)?;
        self.restart_from(restart)?;
        Ok(replace)
    }
}

pub trait DecodeContext: CodecContext {
    fn full_data(&self) -> &[u8];

    fn remaining_data(&self) -> &[u8];
    fn position(&self) -> usize;

    fn advance(&mut self, by: usize);

    fn restart_from(&mut self, pos: usize) -> Result<(), Self::Error>;

    fn error_decoding(&self, byte_range: Range<usize>, reason: Option<&str>) -> Self::Error;

    fn handle_error<E>(
        &mut self,
        errors: &E,
        byte_range: Range<usize>,
        reason: Option<&str>,
    ) -> Result<Self::StrBuf, Self::Error>
    where
        E: DecodeErrorHandler<Self>,
    {
        let (replace, restart) = errors.handle_decode_error(self, byte_range, reason)?;
        self.restart_from(restart)?;
        Ok(replace)
    }
}

pub trait EncodeErrorHandler<Ctx: EncodeContext> {
    fn handle_encode_error(
        &self,
        ctx: &mut Ctx,
        range: Range<StrSize>,
        reason: Option<&str>,
    ) -> Result<(EncodeReplace<Ctx>, StrSize), Ctx::Error>;
}
pub trait DecodeErrorHandler<Ctx: DecodeContext> {
    fn handle_decode_error(
        &self,
        ctx: &mut Ctx,
        byte_range: Range<usize>,
        reason: Option<&str>,
    ) -> Result<(Ctx::StrBuf, usize), Ctx::Error>;
}

pub enum EncodeReplace<Ctx: CodecContext> {
    Str(Ctx::StrBuf),
    Bytes(Ctx::BytesBuf),
}

#[derive(Copy, Clone, Default, Debug)]
pub struct StrSize {
    pub bytes: usize,
    pub chars: usize,
}

fn iter_code_points(w: &Wtf8) -> impl Iterator<Item = (StrSize, CodePoint)> {
    w.code_point_indices()
        .enumerate()
        .map(|(chars, (bytes, c))| (StrSize { bytes, chars }, c))
}

impl ops::Add for StrSize {
    type Output = Self;
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            bytes: self.bytes + rhs.bytes,
            chars: self.chars + rhs.chars,
        }
    }
}

impl ops::AddAssign for StrSize {
    fn add_assign(&mut self, rhs: Self) {
        self.bytes += rhs.bytes;
        self.chars += rhs.chars;
    }
}

struct DecodeError<'a> {
    valid_prefix: &'a str,
    rest: &'a [u8],
    err_len: Option<usize>,
}

/// # Safety
/// `v[..valid_up_to]` must be valid utf8
const unsafe fn make_decode_err(
    v: &[u8],
    valid_up_to: usize,
    err_len: Option<usize>,
) -> DecodeError<'_> {
    let (valid_prefix, rest) = unsafe { v.split_at_unchecked(valid_up_to) };
    let valid_prefix = unsafe { core::str::from_utf8_unchecked(valid_prefix) };
    DecodeError {
        valid_prefix,
        rest,
        err_len,
    }
}

enum HandleResult<'a> {
    Done,
    Error {
        err_len: Option<usize>,
        reason: &'a str,
    },
}

fn decode_utf8_compatible<Ctx, E, DecodeF, ErrF>(
    mut ctx: Ctx,
    errors: &E,
    decode: DecodeF,
    handle_error: ErrF,
) -> Result<(Wtf8Buf, usize), Ctx::Error>
where
    Ctx: DecodeContext,
    E: DecodeErrorHandler<Ctx>,
    DecodeF: Fn(&[u8]) -> Result<&str, DecodeError<'_>>,
    ErrF: Fn(&[u8], Option<usize>) -> HandleResult<'static>,
{
    if ctx.remaining_data().is_empty() {
        return Ok((Wtf8Buf::new(), 0));
    }
    let mut out = Wtf8Buf::with_capacity(ctx.remaining_data().len());
    loop {
        match decode(ctx.remaining_data()) {
            Ok(decoded) => {
                out.push_str(decoded);
                ctx.advance(decoded.len());
                break;
            }
            Err(e) => {
                out.push_str(e.valid_prefix);
                match handle_error(e.rest, e.err_len) {
                    HandleResult::Done => {
                        ctx.advance(e.valid_prefix.len());
                        break;
                    }
                    HandleResult::Error { err_len, reason } => {
                        let err_start = ctx.position() + e.valid_prefix.len();
                        let err_end = match err_len {
                            Some(len) => err_start + len,
                            None => ctx.full_data().len(),
                        };
                        let err_range = err_start..err_end;
                        let replace = ctx.handle_error(errors, err_range, Some(reason))?;
                        out.push_wtf8(replace.as_ref());
                        continue;
                    }
                }
            }
        }
    }
    Ok((out, ctx.position()))
}

#[inline]
fn encode_utf8_compatible<Ctx, E>(
    mut ctx: Ctx,
    errors: &E,
    err_reason: &str,
    target_kind: StrKind,
) -> Result<Vec<u8>, Ctx::Error>
where
    Ctx: EncodeContext,
    E: EncodeErrorHandler<Ctx>,
{
    // let mut data = s.as_ref();
    // let mut char_data_index = 0;
    let mut out = Vec::<u8>::with_capacity(ctx.remaining_data().len());
    loop {
        let data = ctx.remaining_data();
        let mut iter = iter_code_points(data);
        let Some((i, _)) = iter.find(|(_, c)| !target_kind.can_encode(*c)) else {
            break;
        };

        out.extend_from_slice(&ctx.remaining_data().as_bytes()[..i.bytes]);

        let err_start = ctx.position() + i;
        // number of non-compatible chars between the first non-compatible char and the next compatible char
        let err_end = match { iter }.find(|(_, c)| target_kind.can_encode(*c)) {
            Some((i, _)) => ctx.position() + i,
            None => ctx.data_len(),
        };

        let range = err_start..err_end;
        let replace = ctx.handle_error(errors, range.clone(), Some(err_reason))?;
        match replace {
            EncodeReplace::Str(s) => {
                if s.is_compatible_with(target_kind) {
                    out.extend_from_slice(s.as_ref().as_bytes());
                } else {
                    return Err(ctx.error_encoding(range, Some(err_reason)));
                }
            }
            EncodeReplace::Bytes(b) => {
                out.extend_from_slice(b.as_ref());
            }
        }
    }
    out.extend_from_slice(ctx.remaining_data().as_bytes());
    Ok(out)
}

pub mod errors {
    use crate::str::UnicodeEscapeCodepoint;

    use super::*;
    use core::fmt::Write;

    #[derive(Clone, Copy)]
    pub struct Strict;

    impl<Ctx: EncodeContext> EncodeErrorHandler<Ctx> for Strict {
        fn handle_encode_error(
            &self,
            ctx: &mut Ctx,
            range: Range<StrSize>,
            reason: Option<&str>,
        ) -> Result<(EncodeReplace<Ctx>, StrSize), Ctx::Error> {
            Err(ctx.error_encoding(range, reason))
        }
    }

    impl<Ctx: DecodeContext> DecodeErrorHandler<Ctx> for Strict {
        fn handle_decode_error(
            &self,
            ctx: &mut Ctx,
            byte_range: Range<usize>,
            reason: Option<&str>,
        ) -> Result<(Ctx::StrBuf, usize), Ctx::Error> {
            Err(ctx.error_decoding(byte_range, reason))
        }
    }

    #[derive(Clone, Copy)]
    pub struct Ignore;

    impl<Ctx: EncodeContext> EncodeErrorHandler<Ctx> for Ignore {
        fn handle_encode_error(
            &self,
            ctx: &mut Ctx,
            range: Range<StrSize>,
            _reason: Option<&str>,
        ) -> Result<(EncodeReplace<Ctx>, StrSize), Ctx::Error> {
            Ok((EncodeReplace::Bytes(ctx.bytes(b"".into())), range.end))
        }
    }

    impl<Ctx: DecodeContext> DecodeErrorHandler<Ctx> for Ignore {
        fn handle_decode_error(
            &self,
            ctx: &mut Ctx,
            byte_range: Range<usize>,
            _reason: Option<&str>,
        ) -> Result<(Ctx::StrBuf, usize), Ctx::Error> {
            Ok((ctx.string("".into()), byte_range.end))
        }
    }

    #[derive(Clone, Copy)]
    pub struct Replace;

    impl<Ctx: EncodeContext> EncodeErrorHandler<Ctx> for Replace {
        fn handle_encode_error(
            &self,
            ctx: &mut Ctx,
            range: Range<StrSize>,
            _reason: Option<&str>,
        ) -> Result<(EncodeReplace<Ctx>, StrSize), Ctx::Error> {
            let replace = "?".repeat(range.end.chars - range.start.chars);
            Ok((EncodeReplace::Str(ctx.string(replace.into())), range.end))
        }
    }

    impl<Ctx: DecodeContext> DecodeErrorHandler<Ctx> for Replace {
        fn handle_decode_error(
            &self,
            ctx: &mut Ctx,
            byte_range: Range<usize>,
            _reason: Option<&str>,
        ) -> Result<(Ctx::StrBuf, usize), Ctx::Error> {
            Ok((
                ctx.string(char::REPLACEMENT_CHARACTER.to_string().into()),
                byte_range.end,
            ))
        }
    }

    #[derive(Clone, Copy)]
    pub struct XmlCharRefReplace;

    impl<Ctx: EncodeContext> EncodeErrorHandler<Ctx> for XmlCharRefReplace {
        fn handle_encode_error(
            &self,
            ctx: &mut Ctx,
            range: Range<StrSize>,
            _reason: Option<&str>,
        ) -> Result<(EncodeReplace<Ctx>, StrSize), Ctx::Error> {
            let err_str = &ctx.full_data()[range.start.bytes..range.end.bytes];
            let num_chars = range.end.chars - range.start.chars;
            // capacity rough guess; assuming that the codepoints are 3 digits in decimal + the &#;
            let mut out = String::with_capacity(num_chars * 6);
            for c in err_str.code_points() {
                write!(out, "&#{};", c.to_u32()).unwrap()
            }
            Ok((EncodeReplace::Str(ctx.string(out.into())), range.end))
        }
    }

    #[derive(Clone, Copy)]
    pub struct BackslashReplace;

    impl<Ctx: EncodeContext> EncodeErrorHandler<Ctx> for BackslashReplace {
        fn handle_encode_error(
            &self,
            ctx: &mut Ctx,
            range: Range<StrSize>,
            _reason: Option<&str>,
        ) -> Result<(EncodeReplace<Ctx>, StrSize), Ctx::Error> {
            let err_str = &ctx.full_data()[range.start.bytes..range.end.bytes];
            let num_chars = range.end.chars - range.start.chars;
            // minimum 4 output bytes per char: \xNN
            let mut out = String::with_capacity(num_chars * 4);
            for c in err_str.code_points() {
                write!(out, "{}", UnicodeEscapeCodepoint(c)).unwrap();
            }
            Ok((EncodeReplace::Str(ctx.string(out.into())), range.end))
        }
    }

    impl<Ctx: DecodeContext> DecodeErrorHandler<Ctx> for BackslashReplace {
        fn handle_decode_error(
            &self,
            ctx: &mut Ctx,
            byte_range: Range<usize>,
            _reason: Option<&str>,
        ) -> Result<(Ctx::StrBuf, usize), Ctx::Error> {
            let err_bytes = &ctx.full_data()[byte_range.clone()];
            let mut replace = String::with_capacity(4 * err_bytes.len());
            for &c in err_bytes {
                write!(replace, "\\x{c:02x}").unwrap();
            }
            Ok((ctx.string(replace.into()), byte_range.end))
        }
    }

    #[derive(Clone, Copy)]
    pub struct NameReplace;

    impl<Ctx: EncodeContext> EncodeErrorHandler<Ctx> for NameReplace {
        fn handle_encode_error(
            &self,
            ctx: &mut Ctx,
            range: Range<StrSize>,
            _reason: Option<&str>,
        ) -> Result<(EncodeReplace<Ctx>, StrSize), Ctx::Error> {
            let err_str = &ctx.full_data()[range.start.bytes..range.end.bytes];
            let num_chars = range.end.chars - range.start.chars;
            let mut out = String::with_capacity(num_chars * 4);
            for c in err_str.code_points() {
                let c_u32 = c.to_u32();
                if let Some(c_name) = c.to_char().and_then(rustpython_unicode::character_name) {
                    write!(out, "\\N{{{c_name}}}").unwrap();
                } else if c_u32 >= 0x10000 {
                    write!(out, "\\U{c_u32:08x}").unwrap();
                } else if c_u32 >= 0x100 {
                    write!(out, "\\u{c_u32:04x}").unwrap();
                } else {
                    write!(out, "\\x{c_u32:02x}").unwrap();
                }
            }
            Ok((EncodeReplace::Str(ctx.string(out.into())), range.end))
        }
    }

    #[derive(Clone, Copy)]
    pub struct SurrogateEscape;

    impl<Ctx: EncodeContext> EncodeErrorHandler<Ctx> for SurrogateEscape {
        fn handle_encode_error(
            &self,
            ctx: &mut Ctx,
            range: Range<StrSize>,
            reason: Option<&str>,
        ) -> Result<(EncodeReplace<Ctx>, StrSize), Ctx::Error> {
            let err_str = &ctx.full_data()[range.start.bytes..range.end.bytes];
            let num_chars = range.end.chars - range.start.chars;
            let mut out = Vec::with_capacity(num_chars);
            let mut pos = range.start;
            for ch in err_str.code_points() {
                let ch_u32 = ch.to_u32();
                if !(0xdc80..=0xdcff).contains(&ch_u32) {
                    if out.is_empty() {
                        // Can't handle even the first character
                        return Err(ctx.error_encoding(range, reason));
                    }
                    // Return partial result, restart from this character
                    return Ok((EncodeReplace::Bytes(ctx.bytes(out)), pos));
                }
                out.push((ch_u32 - 0xdc00) as u8);
                pos += StrSize {
                    bytes: ch.len_wtf8(),
                    chars: 1,
                };
            }
            Ok((EncodeReplace::Bytes(ctx.bytes(out)), range.end))
        }
    }

    impl<Ctx: DecodeContext> DecodeErrorHandler<Ctx> for SurrogateEscape {
        fn handle_decode_error(
            &self,
            ctx: &mut Ctx,
            byte_range: Range<usize>,
            reason: Option<&str>,
        ) -> Result<(Ctx::StrBuf, usize), Ctx::Error> {
            let err_bytes = &ctx.full_data()[byte_range.clone()];
            let mut consumed = 0;
            let mut replace = Wtf8Buf::with_capacity(4 * byte_range.len());
            while consumed < 4 && consumed < byte_range.len() {
                let c = err_bytes[consumed] as u16;
                // Refuse to escape ASCII bytes
                if c < 128 {
                    break;
                }
                replace.push(CodePoint::from(0xdc00 + c));
                consumed += 1;
            }
            if consumed == 0 {
                return Err(ctx.error_decoding(byte_range, reason));
            }
            Ok((ctx.string(replace), byte_range.start + consumed))
        }
    }
}

pub mod utf8 {
    use super::*;

    pub const ENCODING_NAME: &str = "utf-8";

    #[inline]
    pub fn encode<Ctx, E>(ctx: Ctx, errors: &E) -> Result<Vec<u8>, Ctx::Error>
    where
        Ctx: EncodeContext,
        E: EncodeErrorHandler<Ctx>,
    {
        encode_utf8_compatible(ctx, errors, "surrogates not allowed", StrKind::Utf8)
    }

    pub fn decode<Ctx: DecodeContext, E: DecodeErrorHandler<Ctx>>(
        ctx: Ctx,
        errors: &E,
        final_decode: bool,
    ) -> Result<(Wtf8Buf, usize), Ctx::Error> {
        decode_utf8_compatible(
            ctx,
            errors,
            |v| {
                core::str::from_utf8(v).map_err(|e| {
                    // SAFETY: as specified in valid_up_to's documentation, input[..e.valid_up_to()]
                    //         is valid utf8
                    unsafe { make_decode_err(v, e.valid_up_to(), e.error_len()) }
                })
            },
            |rest, err_len| {
                let first_err = rest[0];
                if matches!(first_err, 0x80..=0xc1 | 0xf5..=0xff) {
                    HandleResult::Error {
                        err_len: Some(1),
                        reason: "invalid start byte",
                    }
                } else if err_len.is_none() {
                    // error_len() == None means unexpected eof
                    if final_decode {
                        HandleResult::Error {
                            err_len,
                            reason: "unexpected end of data",
                        }
                    } else {
                        HandleResult::Done
                    }
                } else if !final_decode && matches!(rest, [0xed, 0xa0..=0xbf]) {
                    // truncated surrogate
                    HandleResult::Done
                } else {
                    HandleResult::Error {
                        err_len,
                        reason: "invalid continuation byte",
                    }
                }
            },
        )
    }
}

pub mod latin_1 {
    use super::*;

    pub const ENCODING_NAME: &str = "latin-1";

    const ERR_REASON: &str = "ordinal not in range(256)";

    #[inline]
    pub fn encode<Ctx, E>(mut ctx: Ctx, errors: &E) -> Result<Vec<u8>, Ctx::Error>
    where
        Ctx: EncodeContext,
        E: EncodeErrorHandler<Ctx>,
    {
        let mut out = Vec::<u8>::new();
        loop {
            let data = ctx.remaining_data();
            let mut iter = iter_code_points(ctx.remaining_data());
            let Some((i, ch)) = iter.find(|(_, c)| !c.is_ascii()) else {
                break;
            };
            out.extend_from_slice(&data.as_bytes()[..i.bytes]);
            let err_start = ctx.position() + i;
            if let Some(byte) = ch.to_u32().to_u8() {
                drop(iter);
                out.push(byte);
                // if the codepoint is between 128..=255, it's utf8-length is 2
                ctx.restart_from(err_start + StrSize { bytes: 2, chars: 1 })?;
            } else {
                // number of non-latin_1 chars between the first non-latin_1 char and the next latin_1 char
                let err_end = match { iter }.find(|(_, c)| c.to_u32() <= 255) {
                    Some((i, _)) => ctx.position() + i,
                    None => ctx.data_len(),
                };
                let err_range = err_start..err_end;
                let replace = ctx.handle_error(errors, err_range.clone(), Some(ERR_REASON))?;
                match replace {
                    EncodeReplace::Str(s) => {
                        if s.as_ref().code_points().any(|c| c.to_u32() > 255) {
                            return Err(ctx.error_encoding(err_range, Some(ERR_REASON)));
                        }
                        out.extend(s.as_ref().code_points().map(|c| c.to_u32() as u8));
                    }
                    EncodeReplace::Bytes(b) => {
                        out.extend_from_slice(b.as_ref());
                    }
                }
            }
        }
        out.extend_from_slice(ctx.remaining_data().as_bytes());
        Ok(out)
    }

    pub fn decode<Ctx: DecodeContext, E: DecodeErrorHandler<Ctx>>(
        ctx: Ctx,
        _errors: &E,
    ) -> Result<(Wtf8Buf, usize), Ctx::Error> {
        let out: String = ctx.remaining_data().iter().map(|c| *c as char).collect();
        let out_len = out.len();
        Ok((out.into(), out_len))
    }
}

pub mod ascii {
    use super::*;
    use ::ascii::AsciiStr;

    pub const ENCODING_NAME: &str = "ascii";

    const ERR_REASON: &str = "ordinal not in range(128)";

    #[inline]
    pub fn encode<Ctx, E>(ctx: Ctx, errors: &E) -> Result<Vec<u8>, Ctx::Error>
    where
        Ctx: EncodeContext,
        E: EncodeErrorHandler<Ctx>,
    {
        encode_utf8_compatible(ctx, errors, ERR_REASON, StrKind::Ascii)
    }

    pub fn decode<Ctx: DecodeContext, E: DecodeErrorHandler<Ctx>>(
        ctx: Ctx,
        errors: &E,
    ) -> Result<(Wtf8Buf, usize), Ctx::Error> {
        decode_utf8_compatible(
            ctx,
            errors,
            |v| {
                AsciiStr::from_ascii(v).map(|s| s.as_str()).map_err(|e| {
                    // SAFETY: as specified in valid_up_to's documentation, input[..e.valid_up_to()]
                    //         is valid ascii & therefore valid utf8
                    unsafe { make_decode_err(v, e.valid_up_to(), Some(1)) }
                })
            },
            |_rest, err_len| HandleResult::Error {
                err_len,
                reason: ERR_REASON,
            },
        )
    }
}

/// General single-byte charmap codec (cp1252, iso8859-x, koi8, ...).
///
/// The standard charmap tables are 256-entry mappings from a byte to a code
/// point. Decoding is a pure table lookup; only unmapped bytes (table value
/// `< 0`) go through the error handler.
pub mod charmap {
    use super::*;

    pub const ERR_REASON: &str = "character maps to <undefined>";

    #[inline]
    fn table_lookup(cp: u32, fast: &[i64; 256], extras: &[(u32, u8)]) -> Option<u8> {
        if cp < 256 {
            let v = fast[cp as usize];
            if v >= 0 {
                return Some(v as u8);
            }
            return None;
        }
        extras
            .iter()
            .find(|(k, _)| *k == cp)
            .map(|(_, v)| *v)
    }

    #[inline]
    fn cp_utf8_len(cp: u32) -> usize {
        if cp < 0x80 {
            1
        } else if cp < 0x800 {
            2
        } else if cp < 0x10000 {
            3
        } else {
            4
        }
    }

    /// Decode bytes through a 256-entry code-point table.
    /// `table[i] < 0` marks byte `i` as unmapped (error).
    #[inline]
    pub fn decode<Ctx, E>(
        mut ctx: Ctx,
        errors: &E,
        table: &[i64; 256],
    ) -> Result<(Wtf8Buf, usize), Ctx::Error>
    where
        Ctx: DecodeContext,
        E: DecodeErrorHandler<Ctx>,
    {
        let mut out = Wtf8Buf::with_capacity(ctx.remaining_data().len());
        loop {
            let data = ctx.remaining_data();
            if data.is_empty() {
                break;
            }
            let Some(first_bad) = data.iter().position(|b| table[*b as usize] < 0) else {
                for b in data {
                    // table values are valid code points (built from 1-char entries)
                    out.push(CodePoint::from_u32(table[*b as usize] as u32).unwrap());
                }
                ctx.advance(data.len());
                break;
            };
            for b in &data[..first_bad] {
                out.push(CodePoint::from_u32(table[*b as usize] as u32).unwrap());
            }
            let err_start = ctx.position() + first_bad;
            let replace =
                ctx.handle_error(errors, err_start..err_start + 1, Some(ERR_REASON))?;
            out.push_wtf8(replace.as_ref());
        }
        Ok((out, ctx.position()))
    }

    /// Encode a string through a single-byte charmap table.
    /// `fast[i]` is the byte for code point `i` (`< 0` = unmapped), and `extras`
    /// covers code points above 255 (e.g. the Euro sign in cp1252).
    #[inline]
    pub fn encode<Ctx, E>(
        mut ctx: Ctx,
        errors: &E,
        fast: &[i64; 256],
        extras: &[(u32, u8)],
    ) -> Result<Vec<u8>, Ctx::Error>
    where
        Ctx: EncodeContext,
        E: EncodeErrorHandler<Ctx>,
    {
        let mut out = Vec::<u8>::new();
        loop {
            let data = ctx.remaining_data();
            let Some((i, ch)) = iter_code_points(data).find(|(_, c)| {
                table_lookup(c.to_u32(), fast, extras).is_none()
            }) else {
                // all remaining code points are mappable: map each one
                // (the mapped byte is not the UTF-8 encoding of the char,
                // so we cannot copy the raw source bytes)
                for (_, c) in iter_code_points(data) {
                    out.push(table_lookup(c.to_u32(), fast, extras).unwrap());
                }
                break;
            };
            // map the valid prefix chars individually (byte length differs
            // from UTF-8 for non-ASCII entries)
            for (pos, c) in iter_code_points(data) {
                if pos.bytes >= i.bytes {
                    break;
                }
                out.push(table_lookup(c.to_u32(), fast, extras).unwrap());
            }
            let err_start = ctx.position() + i;
            let err_range = err_start
                ..err_start
                    + StrSize {
                        bytes: cp_utf8_len(ch.to_u32()),
                        chars: 1,
                    };
            let replace = ctx.handle_error(errors, err_range.clone(), Some(ERR_REASON))?;
            match replace {
                EncodeReplace::Str(s) => {
                    for c in s.as_ref().code_points() {
                        match table_lookup(c.to_u32(), fast, extras) {
                            Some(b) => out.push(b),
                            None => {
                                return Err(ctx.error_encoding(err_range, Some(ERR_REASON)))
                            }
                        }
                    }
                }
                EncodeReplace::Bytes(b) => {
                    out.extend_from_slice(b.as_ref());
                }
            }
        }
        Ok(out)
    }
}
