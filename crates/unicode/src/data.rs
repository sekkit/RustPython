//! Access to the Unicode character database (`unicodedata`).
//!
//! Owns the generated Unicode 3.2.0 / 16.0.0 tables. The latest tables are
//! generated from the UCD 16.0.0 files (the version bundled by CPython 3.14)
//! so that the `unicodedata` module matches CPython's behavior exactly.

// spell-checker:ignore codep decomp DECOMP unidata

use core::{cmp::Ordering, fmt::Write, hint::cold_path};

use alloc::{
    borrow::ToOwned,
    format,
    string::String,
};

use icu_properties::props::{
    BidiClass, CanonicalCombiningClass, EastAsianWidth, GeneralCategory,
    NamedEnumeratedProperty, NumericType,
};
use rustpython_wtf8::CodePoint;

include!(concat!(env!("OUT_DIR"), "/generated/unicode_3_2.rs"));
include!(concat!(env!("OUT_DIR"), "/generated/unicode_latest.rs"));
include!(concat!(env!("OUT_DIR"), "/generated/unicode_num_type.rs"));
include!(concat!(
    env!("OUT_DIR"),
    "/generated/unicode_numeric_value.rs"
));

#[derive(Clone, Copy, PartialEq, Eq)]
enum DecompositionType {
    Canonical,
    Compat,
    Circle,
    Final,
    Font,
    Fraction,
    Initial,
    Isolated,
    Medial,
    Narrow,
    Nobreak,
    Small,
    Square,
    Sub,
    Super,
    Vertical,
    Wide,
}

impl DecompositionType {
    const fn type_tag(self) -> &'static str {
        match self {
            Self::Canonical => "",
            Self::Compat => "compat",
            Self::Circle => "circle",
            Self::Final => "final",
            Self::Font => "font",
            Self::Fraction => "fraction",
            Self::Initial => "initial",
            Self::Isolated => "isolated",
            Self::Medial => "medial",
            Self::Narrow => "narrow",
            Self::Nobreak => "noBreak",
            Self::Small => "small",
            Self::Square => "square",
            Self::Sub => "sub",
            Self::Super => "super",
            Self::Vertical => "vertical",
            Self::Wide => "wide",
        }
    }
}

fn lookup_property<T: Copy>(table: &[(u32, u32, T)], ch: char) -> Option<T> {
    let ch = ch as u32;
    table
        .binary_search_by(|&(start, end, _)| {
            if ch > end {
                Ordering::Less
            } else if ch < start {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        })
        .ok()
        .map(|i| table[i].2)
}

fn lookup_numeric_val(ch: char, modern: bool) -> Option<f64> {
    if modern {
        lookup_property(NUMERIC_VALUES, ch)
    } else {
        cold_path();
        lookup_property(NUMERIC_VALUES_DIFF, ch).or_else(|| {
            NUMERIC_VAL_EXISTS_32
                .binary_search_by(|&(start, end)| {
                    let ch = ch as u32;
                    if ch > end {
                        Ordering::Less
                    } else if ch < start {
                        Ordering::Greater
                    } else {
                        Ordering::Equal
                    }
                })
                .ok()
                .and_then(|_| lookup_property(NUMERIC_VALUES, ch))
        })
    }
}

/// The version string of the latest Unicode database bundled with the
/// standard library (`unicodedata.unidata_version`): UCD 16.0.0, the version
/// CPython 3.14 ships.
#[must_use]
pub fn unicode_version() -> String {
    "16.0.0".to_owned()
}

/// The Unicode name of `ch` (`unicodedata.name`), if any.
#[must_use]
pub fn character_name(ch: char) -> Option<String> {
    Ucd::new(true).character_name(CodePoint::from(ch))
}

/// `unicodedata.lookup` on the latest UCD.
#[must_use]
pub fn lookup_character(name: &str) -> Option<char> {
    Ucd::new(true).lookup_character(name)
}

// ---------------------------------------------------------------------------
// Hangul syllable decomposition and naming (Unicode 3.12)
// ---------------------------------------------------------------------------

const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT; // 588
const S_COUNT: u32 = L_COUNT * N_COUNT; // 11172

// Leading consonants, vowels and trailing consonants of the syllable names,
// matching CPython's `hangul_syllables` table in Modules/unicodedata.c.
const HANGUL_L: [&str; 19] = [
    "G", "GG", "N", "D", "DD", "R", "M", "B", "BB", "S", "SS", "", "J", "JJ", "C", "K", "T",
    "P", "H",
];
const HANGUL_V: [&str; 21] = [
    "A", "AE", "YA", "YAE", "EO", "E", "YEO", "YE", "O", "WA", "WAE", "OE", "YO", "U", "WEO",
    "WE", "WI", "YU", "EU", "YI", "I",
];
const HANGUL_T: [&str; 28] = [
    "", "G", "GG", "GS", "N", "NJ", "NH", "D", "L", "LG", "LM", "LB", "LS", "LT", "LP", "LH",
    "M", "B", "BS", "S", "SS", "NG", "J", "C", "K", "T", "P", "H",
];

/// Full LVT decomposition of a Hangul syllable (Unicode 3.12 "Hangul
/// Syllable Decomposition"), like CPython's `_PyUnicode_Decomposition`.
fn hangul_decomposition(code: u32) -> String {
    let s_index = code - S_BASE;
    let l = L_BASE + s_index / N_COUNT;
    let v = V_BASE + (s_index % N_COUNT) / T_COUNT;
    let t = T_BASE + s_index % T_COUNT;
    if t == T_BASE {
        format!("{l:04X} {v:04X}")
    } else {
        format!("{l:04X} {v:04X} {t:04X}")
    }
}

fn hangul_syllable_name(code: u32) -> String {
    let s_index = code - S_BASE;
    let l = (s_index / N_COUNT) as usize;
    let v = ((s_index % N_COUNT) / T_COUNT) as usize;
    let t = (s_index % T_COUNT) as usize;
    format!(
        "HANGUL SYLLABLE {}{}{}",
        HANGUL_L[l],
        HANGUL_V[v],
        HANGUL_T[t]
    )
}

/// Mirrors CPython's `derived_name_ranges` / `find_prefix_id` in
/// Modules/unicodedata.c: 0 = Hangul, 1 = CJK, 2 = Tangut.
fn derived_name_range(code: u32) -> Option<u8> {
    const RANGES: [(u32, u32, u8); 13] = [
        (0x3400, 0x4DBF, 1),
        (0x4E00, 0x9FFF, 1),
        (0xAC00, 0xD7A3, 0),
        (0x17000, 0x187F7, 2),
        (0x18D00, 0x18D08, 2),
        (0x20000, 0x2A6DF, 1),
        (0x2A700, 0x2B739, 1),
        (0x2B740, 0x2B81D, 1),
        (0x2B820, 0x2CEA1, 1),
        (0x2CEB0, 0x2EBE0, 1),
        (0x2EBF0, 0x2EE5D, 1),
        (0x30000, 0x3134A, 1),
        (0x31350, 0x323AF, 1),
    ];
    for (first, last, id) in RANGES {
        if code < first {
            return None;
        }
        if code <= last {
            return Some(id);
        }
    }
    None
}

/// Match the longest jamo name from `column` at the start of `name`
/// (case-insensitive), returning `(match_len, index)`. Empty jamo entries
/// (e.g. the zero leading consonant or a missing trailing consonant) match
/// only when nothing longer has, mirroring CPython's `find_syllable` (which
/// starts from a `-1` sentinel so a zero-length entry can win).
fn find_syllable(name: &str, column: &[&str]) -> (usize, Option<usize>) {
    let mut best_len: Option<usize> = None;
    let mut best_pos = None;
    for (i, s) in column.iter().enumerate() {
        let len1 = s.len();
        if let Some(best) = best_len
            && len1 <= best
        {
            continue;
        }
        if len1 > name.len() || !name[..len1].eq_ignore_ascii_case(s) {
            continue;
        }
        best_len = Some(len1);
        best_pos = Some(i);
    }
    (best_len.unwrap_or(0), best_pos)
}

/// Parse the hex tail of an algorithmic name, like CPython's
/// `parse_hex_code` (4..6 hex digits, no leading zero).
fn parse_hex_code(tail: &str) -> Option<u32> {
    if tail.len() < 4 || tail.len() > 6 || tail.starts_with('0') {
        return None;
    }
    let mut v: u32 = 0;
    for c in tail.bytes() {
        v = v.checked_mul(16)?;
        v = v.checked_add(match c {
            b'0'..=b'9' => (c - b'0') as u32,
            b'A'..=b'F' => (c - b'A' + 10) as u32,
            b'a'..=b'f' => (c - b'a' + 10) as u32,
            _ => return None,
        })?;
    }
    (v <= 0x10FFFF).then_some(v)
}

fn lookup_name(table: &[(u32, &str)], code: u32) -> Option<String> {
    table
        .binary_search_by_key(&code, |&(cp, _)| cp)
        .ok()
        .map(|i| table[i].1.to_owned())
}

fn lookup_name_by_name(table: &[(&str, u32)], name: &str) -> Option<u32> {
    table
        .binary_search_by(|&(n, _)| n.cmp(name))
        .ok()
        .map(|i| table[i].1)
}

/// A view over the Unicode character database at a fixed version.
///
/// `modern` selects the latest bundled UCD (16.0.0); otherwise the Unicode
/// 3.2.0 tables used by `unicodedata.ucd_3_2_0` are consulted.
#[derive(Debug, Clone, Copy)]
pub struct Ucd {
    modern: bool,
}

impl Ucd {
    #[must_use]
    pub const fn new(modern: bool) -> Self {
        Self { modern }
    }

    #[must_use]
    pub fn category(&self, c: CodePoint) -> &'static str {
        let Some(c) = c.to_char() else {
            return GeneralCategory::Surrogate.short_name();
        };
        if self.modern {
            lookup_property(GENERAL_CATEGORY_LATEST, c)
        } else {
            cold_path();
            lookup_property(GENERAL_CATEGORY, c)
        }
        .unwrap_or(GeneralCategory::Unassigned)
        .short_name()
    }

    #[must_use]
    pub fn bidirectional(&self, c: CodePoint) -> &'static str {
        let Some(c) = c.to_char() else {
            return BidiClass::LeftToRight.short_name();
        };
        // CPython leaves the bidi class empty for unassigned code points.
        if self.category(CodePoint::from(c)) == "Cn" {
            return "";
        }
        if self.modern {
            lookup_property(BIDI_CLASS_LATEST, c)
        } else {
            cold_path();
            lookup_property(BIDI_CLASS, c)
        }
        .unwrap_or(BidiClass::LeftToRight)
        .short_name()
    }

    #[must_use]
    pub fn east_asian_width(&self, c: CodePoint) -> &'static str {
        let Some(c) = c.to_char() else {
            return EastAsianWidth::Neutral.short_name();
        };
        if self.modern {
            return lookup_property(EAST_ASIAN_WIDTH_LATEST, c)
                .unwrap_or(EastAsianWidth::Neutral)
                .short_name();
        }
        cold_path();
        if lookup_property(GENERAL_CATEGORY, c).is_some() {
            // Assigned in 3.2.0: the 3.2.0 width (missing = old default N).
            lookup_property(EAST_ASIAN_WIDTH, c)
                .unwrap_or(EastAsianWidth::Neutral)
                .short_name()
        } else if lookup_property(GENERAL_CATEGORY_LATEST, c).is_some() {
            // Unassigned in 3.2.0, assigned now: CPython's default width.
            EastAsianWidth::Neutral.short_name()
        } else {
            // Unassigned in both versions: no change record → modern value.
            lookup_property(EAST_ASIAN_WIDTH_LATEST, c)
                .unwrap_or(EastAsianWidth::Neutral)
                .short_name()
        }
    }

    #[must_use]
    pub fn mirrored(&self, c: CodePoint) -> i32 {
        c.to_char().map_or(0, |c| {
            let c = c as u32;
            let table = if self.modern {
                BIDI_MIRRORED_LATEST
            } else {
                cold_path();
                BIDI_MIRRORED
            };
            table
                .binary_search_by(|&(start, end)| {
                    if c > end {
                        Ordering::Less
                    } else if c < start {
                        Ordering::Greater
                    } else {
                        Ordering::Equal
                    }
                })
                .is_ok() as i32
        })
    }

    #[must_use]
    pub fn combining(&self, c: CodePoint) -> u8 {
        c.to_char()
            .and_then(|c| {
                if self.modern {
                    lookup_property(COMBINING_CLASS_LATEST, c)
                } else {
                    cold_path();
                    lookup_property(COMBINING_CLASS, c).map(|ccc| ccc.to_icu4c_value())
                }
            })
            .unwrap_or(CanonicalCombiningClass::NotReordered.to_icu4c_value())
    }

    #[must_use]
    pub fn decomposition(&self, c: CodePoint) -> String {
        let Some(ch) = c.to_char() else {
            return String::new();
        };
        let code = ch as u32;

        // Unassigned in 3.2.0 has no decomposition in the 3.2.0 view, even if
        // the character gained one later (CPython: category_changed == 0).
        if !self.modern
            && lookup_property(GENERAL_CATEGORY, ch).is_none()
            && lookup_property(GENERAL_CATEGORY_LATEST, ch).is_some()
        {
            return String::new();
        }

        // Hangul syllables decompose algorithmically to the full LVT, like
        // CPython's _PyUnicode_Decomposition. Both views share the same
        // table for the rest (CPython does the same).
        if (S_BASE..S_BASE + S_COUNT).contains(&code) {
            return hangul_decomposition(code);
        }

        // The decomposition table from UnicodeData.txt (canonical entries
        // without tag, compatibility entries with "<tag>"). Note that
        // UnicodeData.txt already contains the corrected decompositions for
        // the characters in NormalizationCorrections.txt.
        if let Ok(i) = DECOMP.binary_search_by_key(&code, |&(codep, _, _)| codep) {
            let tag = DECOMP[i].1;
            let end = DECOMP[i].2;
            let start = i
                .checked_sub(1)
                .map(|i| DECOMP[i].2)
                .unwrap_or_default();

            let decomp = &DECOMP_RANGE[start..end];
            let cap = decomp.len() * 6 + tag.type_tag().len() + 2;
            let mut out = String::with_capacity(cap);

            if tag != DecompositionType::Canonical {
                write!(out, "<{}>", tag.type_tag()).unwrap();
            }
            for (j, ch) in decomp.iter().enumerate() {
                if j > 0 || tag != DecompositionType::Canonical {
                    out.push(' ');
                }
                write!(out, "{ch:04X}").unwrap();
            }

            out
        } else {
            String::new()
        }
    }

    /// The Unicode name of `c` (`unicodedata.name`), if any.
    #[must_use]
    pub fn character_name(&self, c: CodePoint) -> Option<String> {
        let ch = c.to_char()?;
        let code = ch as u32;
        if !self.modern
            && lookup_property(GENERAL_CATEGORY, ch).is_none()
            && lookup_property(GENERAL_CATEGORY_LATEST, ch).is_some()
        {
            // Unassigned in 3.2.0 but assigned now: no name (CPython's
            // category_changed == 0).
            return None;
        }
        if let Some(id) = derived_name_range(code) {
            return match id {
                0 => Some(hangul_syllable_name(code)),
                1 => Some(format!("CJK UNIFIED IDEOGRAPH-{code:04X}")),
                // Tangut ideograph names are a CPython 3.15 feature; 3.14's
                // name() leaves them unnamed.
                _ => None,
            };
        }
        // The 3.2.0 and 16.0.0 names are identical for assigned characters,
        // so both views share the modern name table.
        lookup_name(NAMES, code)
    }

    /// `unicodedata.lookup` on this view.
    #[must_use]
    pub fn lookup_character(&self, name: &str) -> Option<char> {
        let name_upper = name.to_ascii_uppercase();

        if let Some(rest) = name_upper.strip_prefix("HANGUL SYLLABLE ") {
            let (l_len, l) = find_syllable(rest, &HANGUL_L);
            let rest = &rest[l_len..];
            let (v_len, v) = find_syllable(rest, &HANGUL_V);
            let rest = &rest[v_len..];
            let (t_len, t) = find_syllable(rest, &HANGUL_T);
            let rest = &rest[t_len..];
            if let (Some(l), Some(v), Some(t)) = (l, v, t)
                && rest.is_empty()
            {
                let code = S_BASE + ((l as u32 * V_COUNT + v as u32) * T_COUNT) + t as u32;
                return char::from_u32(code);
            }
            return None;
        }

        for (prefix, id) in [
            ("CJK UNIFIED IDEOGRAPH-", 1u8),
            ("TANGUT IDEOGRAPH-", 2u8),
        ] {
            if let Some(tail) = name_upper.strip_prefix(prefix)
                && let Some(v) = parse_hex_code(tail)
                && derived_name_range(v) == Some(id)
            {
                return char::from_u32(v);
            }
        }

        let table = NAMES_BY_NAME;
        lookup_name_by_name(table, &name_upper).and_then(char::from_u32)
    }

    fn numeric_type_matches(self, ch: CodePoint, expected: &[NumericType]) -> Option<char> {
        let ch = ch.to_char()?;

        let actual = if self.modern {
            lookup_property(NUMERIC_TYPE_LATEST, ch)
        } else {
            cold_path();
            lookup_property(NUMERIC_TYPE_DIFF, ch)
                .or_else(|| lookup_property(NUMERIC_TYPE_LATEST, ch))
        };

        expected.contains(&actual?).then_some(ch)
    }

    /// The integer digit value of `c` (`unicodedata.digit`), if it has one.
    #[must_use]
    pub fn digit(&self, c: CodePoint) -> Option<u64> {
        let expected = [NumericType::Decimal, NumericType::Digit];
        self.numeric_type_matches(c, &expected).and_then(|ch| {
            let value = lookup_numeric_val(ch, true)?;
            let int = value as u64;
            (int as f64 == value).then_some(int)
        })
    }

    /// The integer decimal value of `c` (`unicodedata.decimal`), if it has one.
    #[must_use]
    pub fn decimal(&self, c: CodePoint) -> Option<u64> {
        let expected = [NumericType::Decimal];
        self.numeric_type_matches(c, &expected).and_then(|ch| {
            let value = lookup_numeric_val(ch, self.modern)?;
            let int = value as u64;
            (int as f64 == value).then_some(int)
        })
    }

    /// The numeric value of `c` (`unicodedata.numeric`), if it has one.
    #[must_use]
    pub fn numeric(&self, c: CodePoint) -> Option<f64> {
        if self.modern {
            let expected = &NumericType::ALL_VALUES[1..];
            return self
                .numeric_type_matches(c, expected)
                .and_then(|ch| lookup_numeric_val(ch, true));
        }
        cold_path();
        // The 3.2.0 view: `numeric()` doesn't check the numeric type (Unihan
        // values carry no type in the UCD), and chars unassigned in 3.2.0
        // have no numeric (CPython: category_changed == 0).
        let ch = c.to_char()?;
        lookup_property(GENERAL_CATEGORY, ch)?;
        lookup_numeric_val(ch, false)
    }

    #[must_use]
    pub fn unidata_version(&self) -> String {
        if self.modern {
            unicode_version()
        } else {
            "3.2.0".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use rustpython_wtf8::CodePoint;

    use super::{Ucd, character_name, lookup_character};

    fn cp(ch: char) -> CodePoint {
        CodePoint::from(ch)
    }

    #[test]
    fn data_queries_match_unicodedata_behavior() {
        let ucd = Ucd::new(true);
        assert_eq!(ucd.category(cp('A')), "Lu");
        assert_eq!(ucd.category(CodePoint::from_u32(0xD800).unwrap()), "Cs");
        assert_eq!(lookup_character("SNOWMAN"), Some('☃'));
        assert_eq!(lookup_character("snowman"), Some('☃'));
        assert_eq!(character_name('☃').as_deref(), Some("SNOWMAN"));
        assert_eq!(ucd.decimal(cp('५')), Some(5));
        assert_eq!(ucd.digit(cp('²')), Some(2));
        let third = ucd.numeric(cp('⅓')).unwrap();
        assert!((third - 1.0 / 3.0).abs() < 1e-6, "got {third}");
        // Exact rational values, matching CPython's double arithmetic.
        assert_eq!(ucd.numeric(cp('⅐')), Some(1.0 / 7.0));
        // Unassigned code points have an empty bidi class.
        assert_eq!(ucd.bidirectional(CodePoint::from_u32(0x0378).unwrap()), "");
        // Hangul syllables decompose to the full LVT.
        assert_eq!(ucd.decomposition(cp('가')), "1100 1161");
        assert_eq!(ucd.decomposition(cp('각')), "1100 1161 11A8");
        // Algorithmic CJK names and reverse lookup.
        assert_eq!(
            character_name('一').as_deref(),
            Some("CJK UNIFIED IDEOGRAPH-4E00")
        );
        assert_eq!(lookup_character("CJK UNIFIED IDEOGRAPH-4e00"), Some('一'));
        assert_eq!(
            character_name('\u{AC01}').as_deref(),
            Some("HANGUL SYLLABLE GAG")
        );
        assert_eq!(lookup_character("hangul syllable ga"), Some('가'));
        // Tangut names are not supported by name() in CPython 3.14.
        assert_eq!(character_name('\u{17000}'), None);
        assert_eq!(lookup_character("TANGUT IDEOGRAPH-17000"), Some('\u{17000}'));
        // Unassigned in 16.0.0 (assigned in 17.0.0).
        assert_eq!(character_name('\u{1ACF}'), None);
        assert_eq!(ucd.category(cp('\u{1ACF}')), "Cn");
        // Decomposition format: space-separated, no leading space; compat
        // entries carry the "<tag>".
        assert_eq!(ucd.decomposition(cp('À')), "0041 0300");
        assert_eq!(ucd.decomposition(cp('¼')), "<fraction> 0031 2044 0034");
        // East Asian width for unassigned chars defaults to Neutral in 16.0.0.
        assert_eq!(ucd.east_asian_width(CodePoint::from_u32(0x0378).unwrap()), "N");
    }

    #[test]
    fn ucd_3_2_0_view_differs_from_modern() {
        let legacy = Ucd::new(false);
        assert_eq!(legacy.unidata_version(), "3.2.0");
        // U+0221 was unassigned in 3.2.0.
        assert_eq!(legacy.character_name(CodePoint::from('\u{0221}')), None);
        assert_eq!(legacy.category(cp('A')), "Lu");

        // Unassigned in 3.2.0 but assigned now: default values (CPython's
        // category_changed == 0).
        let now_assigned = CodePoint::from('\u{4DB6}');
        assert_eq!(legacy.character_name(now_assigned), None);
        assert_eq!(legacy.decomposition(now_assigned), "");
        assert_eq!(legacy.east_asian_width(now_assigned), "N");
        assert_eq!(legacy.numeric(now_assigned), None);

        // Unassigned in both versions: no change record → modern values
        // (FA6E is a reserved CJK range with width W in modern).
        let both_unassigned = CodePoint::from_u32(0xFA6E).unwrap();
        assert_eq!(legacy.east_asian_width(both_unassigned), "W");
        assert_eq!(legacy.character_name(both_unassigned), None);

        // Unihan-3.2.0 numeric values: CJK ideographs carry them even though
        // the UCD-derived 3.2.0 files don't.
        assert_eq!(legacy.numeric(cp('一')), Some(1.0));
        assert_eq!(legacy.numeric(CodePoint::from('\u{5793}')), Some(1e20));
        // A character whose numeric was added after 3.2.0 has no 3.2.0 value.
        assert_eq!(legacy.numeric(CodePoint::from('\u{9F8}')), None);

        // lookup() is version-independent: it finds modern names too.
        assert_eq!(legacy.lookup_character("CJK UNIFIED IDEOGRAPH-4DB6"), Some('\u{4DB6}'));
    }
}
