// spell-checker:ignore decomp DECOMP

extern crate alloc;

use core::num::NonZeroUsize;

use alloc::collections::{BTreeMap, BTreeSet};

use std::{
    env,
    fs::{self, File},
    io::{self, BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    thread,
};

use icu_properties::props::GeneralCategory;

fn generate_unicode_3_2() {
    let path = PathBuf::from(env::var("OUT_DIR").unwrap())
        .join("generated")
        .join("unicode_3_2.rs");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = BufWriter::new(File::create(&path).unwrap());

    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("unicode")
        .join("ucd32");

    write_derived(
        &base,
        "DerivedGeneralCategory-3.2.0.txt",
        "GENERAL_CATEGORY",
        "(u32, u32, GeneralCategory)",
        NonZeroUsize::new(1).unwrap(),
        &mut writer,
        |start, end, id, _| {
            let id = parse_general(id);
            if id != GeneralCategory::Unassigned {
                Some((start, end, id))
            } else {
                None
            }
        },
        |writer, mut values| {
            values.sort_unstable_by_key(|(start, _, _)| *start);
            write!(writer, "[").unwrap();
            for (start, end, id) in values {
                write!(writer, "({start}, {end}, GeneralCategory::{id:?}),").unwrap();
            }
            write!(writer, "];").unwrap();
        },
    );

    write_derived(
        &base,
        "DerivedEastAsianWidth-3.2.0.txt",
        "EAST_ASIAN_WIDTH",
        "(u32, u32, EastAsianWidth)",
        NonZeroUsize::new(1).unwrap(),
        &mut writer,
        |start, end, id, _| {
            let id = parse_eaw(id);
            if id != "EastAsianWidth::Neutral" {
                Some((start, end, id))
            } else {
                None
            }
        },
        |writer, mut values| {
            values.sort_unstable_by_key(|(start, _, _)| *start);
            write!(writer, "[").unwrap();
            for (start, end, id) in values {
                write!(writer, "({start}, {end}, {id}),").unwrap();
            }
            write!(writer, "];").unwrap();
        },
    );

    write_derived(
        &base,
        "DerivedBidiClass-3.2.0.txt",
        "BIDI_CLASS",
        "(u32, u32, BidiClass)",
        NonZeroUsize::new(1).unwrap(),
        &mut writer,
        |start, end, id, _| {
            let id = parse_bidi(id);
            if id != "BidiClass::LeftToRight" {
                Some((start, end, id))
            } else {
                None
            }
        },
        |writer, mut values| {
            values.sort_unstable_by_key(|(start, _, _)| *start);
            write!(writer, "[").unwrap();
            for (start, end, id) in values {
                write!(writer, "({start}, {end}, {id}),").unwrap();
            }
            write!(writer, "];").unwrap();
        },
    );

    write_derived(
        &base,
        "DerivedBinaryProperties-3.2.0.txt",
        "BIDI_MIRRORED",
        "(u32, u32)",
        NonZeroUsize::new(1).unwrap(),
        &mut writer,
        |start, end, id, _| {
            assert_eq!(
                "Bidi_Mirrored",
                id.trim(),
                "DerivedBinaryProperties-3.2.0 only has Bidi_Mirrored"
            );
            Some((start, end))
        },
        |writer, mut values| {
            values.sort_unstable_by_key(|(start, _)| *start);
            writeln!(writer, "{values:?};").unwrap();
        },
    );

    write_derived(
        &base,
        "DerivedCombiningClass-3.2.0.txt",
        "COMBINING_CLASS",
        "(u32, u32, CanonicalCombiningClass)",
        NonZeroUsize::new(1).unwrap(),
        &mut writer,
        |start, end, id, _| {
            let id: u8 = id.parse().unwrap();
            if id == 0 {
                return None;
            }
            Some((start, end, id))
        },
        |writer, mut values| {
            values.sort_unstable_by_key(|(start, _, _)| *start);
            write!(writer, "[").unwrap();
            for (start, end, id) in values {
                write!(
                    writer,
                    "({start}, {end}, CanonicalCombiningClass::from_icu4c_value({id})),"
                )
                .unwrap();
            }
            writeln!(writer, "];").unwrap();
        },
    );
}

fn generate_numeric_type() {
    let path = PathBuf::from(env::var("OUT_DIR").unwrap())
        .join("generated")
        .join("unicode_num_type.rs");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = BufWriter::new(File::create(&path).unwrap());

    let ucd32 = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("unicode")
        .join("ucd32");
    let latest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("unicode")
        .join("latest");

    // Build a map from the 16.0.0 DerivedNumericType.txt so that the
    // 3.2.0 DIFF table compares against the modern UCD version.
    let mut modern_map = Vec::<(u32, u32, String)>::new();
    write_derived(
        &latest,
        "DerivedNumericType.txt",
        "NUMERIC_TYPE_LATEST", // dummy — we only need the parsed values
        "(u32, u32, NumericType)",
        NonZeroUsize::new(1).unwrap(),
        &mut io::sink(),
        |start, end, id, _| {
            Some((start, end, id.to_owned()))
        },
        |_writer, values| {
            modern_map = values;
        },
    );

    // The 3.2.0 DIFF table: entries whose type differs from the 16.0.0 type.
    write_derived(
        &ucd32,
        "DerivedNumericType-3.2.0.txt",
        "NUMERIC_TYPE_DIFF",
        "(u32, u32, NumericType)",
        NonZeroUsize::new(1).unwrap(),
        &mut writer,
        |start, end, id, _| {
            let id = parse_numeric_type_str(id);
            let differs = (start..=end).any(|c| {
                let modern_id = lookup_modern_type(c, &modern_map);
                modern_id != id
            });
            if differs {
                Some((start, end, id))
            } else {
                None
            }
        },
        |writer, mut values| {
            values.sort_unstable_by_key(|(start, _, _)| *start);
            write!(writer, "[").unwrap();
            for (start, end, id) in values {
                write!(writer, "({start}, {end}, {id}),").unwrap();
            }
            writeln!(writer, "];").unwrap();
        },
    );
}

fn lookup_modern_type(c: u32, map: &[(u32, u32, String)]) -> &str {
    for (start, end, id) in map {
        if c >= *start && c <= *end {
            return id;
        }
    }
    "None"
}

fn generate_numeric_value() {
    let path = PathBuf::from(env::var("OUT_DIR").unwrap())
        .join("generated")
        .join("unicode_numeric_value.rs");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = BufWriter::new(File::create(&path).unwrap());

    // Ideally, this would store the diffs between the two tables. However, we need 3.2.0
    // membership as well as different chars. The final tables are both smaller than storing the
    // full 3.2.0 value table.
    let ucd32 = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("unicode")
        .join("ucd32");
    let mut ucd32_diffs = BTreeMap::new();
    let mut ucd32_member = BTreeSet::new();
    // The 3.2.0 membership comes from the derived file (it includes the Unihan-derived
    // values), while the values themselves come from UnicodeData-3.2.0.txt field 8 (the
    // rational, like "1/3"); the derived 3.2.0 file only stores truncated decimals. CPython
    // parses the rational the same way in makeunicodedata.py.
    {
        let numeric_32 =
            BufReader::new(File::open(ucd32.join("DerivedNumericValues-3.2.0.txt")).unwrap());
        parse_unicode_3_2(
            numeric_32,
            NonZeroUsize::new(1).unwrap(),
            &mut io::empty(),
            |start, end, value, _| {
                if !value.trim().is_empty() {
                    ucd32_member.insert((start, end));
                }
                Option::<()>::None
            },
            |_writer, _values| {},
        );
    }
    let numeric_32 = BufReader::new(File::open(ucd32.join("UnicodeData-3.2.0.txt")).unwrap());
    parse_unicode_3_2(
        numeric_32,
        NonZeroUsize::new(8).unwrap(),
        &mut io::empty(),
        |start, end, value, _| {
            if !value.trim().is_empty() {
                let value = parse_rational(value);
                ucd32_diffs.insert((start, end), value);
            }
            Option::<()>::None
        },
        |_writer, _values| {},
    );

    // Unihan-3.2.0 numeric values (kAccountingNumeric / kPrimaryNumeric /
    // kOtherNumeric), which CPython's makeunicodedata.py patches onto the
    // 3.2.0 numeric field. The UCD-derived 3.2.0 files don't carry them, but
    // `ucd_3_2_0.numeric()` returns them (e.g. CJK UNIFIED IDEOGRAPH-4E00 = 1.0).
    {
        let unihan = BufReader::new(File::open(ucd32.join("UnihanNumericValues-3.2.0.txt")).unwrap());
        for line in unihan.lines().map(Result::unwrap) {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_whitespace();
            let cp = u32::from_str_radix(fields.next().unwrap().trim_start_matches("U+"), 16).unwrap();
            let _tag = fields.next().unwrap();
            let value = parse_rational(fields.next().unwrap().replace(',', "").as_str());
            ucd32_diffs.insert((cp, cp), value);
            ucd32_member.insert((cp, cp));
        }
    }

    let ucd_latest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("unicode")
        .join("latest");

    // The latest DerivedNumericValues.txt stores each value twice: field 1 is a truncated
    // decimal approximation and field 3 is the exact rational (e.g. "1/7"). Parse the
    // rational like CPython does so that numeric() returns the exact double.
    write_derived(
        &ucd_latest,
        "DerivedNumericValues.txt",
        "NUMERIC_VALUES",
        "(u32, u32, f64)",
        NonZeroUsize::new(3).unwrap(),
        &mut writer,
        |start, end, value, _| {
            let value = parse_rational(value);

            if ucd32_diffs
                .get(&(start, end))
                .is_some_and(|old_v| *old_v == value)
            {
                ucd32_diffs.remove(&(start, end));
            }

            Some((start, end, value))
        },
        |writer, mut values| {
            values.sort_unstable_by_key(|(ch, _, _)| *ch);
            writeln!(writer, "{values:?};").unwrap();
        },
    );

    // TODO: More flexible parser
    writeln!(
        writer,
        "static NUMERIC_VALUES_DIFF: &[(u32, u32, f64)] = &["
    )
    .unwrap();
    for ((start, end), value) in ucd32_diffs {
        write!(writer, "({start}, {end}, {value:?}),").unwrap();
    }
    writeln!(writer, "];").unwrap();

    // Compress membership table
    let mut iter = ucd32_member.iter();
    let &(mut start_prev, mut end_prev) = iter.next().unwrap();
    let mut membership = Vec::new();

    for &(start, end) in iter {
        if start <= end_prev + 1 {
            end_prev = end_prev.max(end);
        } else {
            membership.push((start_prev, end_prev));
            start_prev = start;
            end_prev = end;
        }
    }
    membership.push((start_prev, end_prev));
    membership.sort_unstable_by_key(|&(start, _)| start);

    writeln!(writer, "static NUMERIC_VAL_EXISTS_32: &[(u32, u32)] = &").unwrap();
    write!(writer, "{membership:?};").unwrap();
}

/// Parse a UCD numeric value: an integer, or a fraction like "1/7".
fn parse_rational(value: &str) -> f64 {
    match value.split_once('/') {
        Some((num, den)) => {
            let num: f64 = num.trim().parse().expect("Unicode data contains valid properties");
            let den: f64 = den.trim().parse().expect("Unicode data contains valid properties");
            num / den
        }
        None => value
            .trim()
            .parse()
            .expect("Unicode data contains valid properties"),
    }
}

fn generate_unicode_latest() {
    let path = PathBuf::from(env::var("OUT_DIR").unwrap())
        .join("generated")
        .join("unicode_latest.rs");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = BufWriter::new(File::create(&path).unwrap());

    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("unicode")
        .join("latest");

    // Unicode 16.0.0 (the version bundled by CPython 3.14) tables generated
    // from the UCD derived files, so the modern view matches CPython's
    // unicodedata exactly. Unassigned / default-valued code points are left
    // out and resolved by the lookup fallbacks in data.rs.

    write_derived(
        &base,
        "DerivedGeneralCategory.txt",
        "GENERAL_CATEGORY_LATEST",
        "(u32, u32, GeneralCategory)",
        NonZeroUsize::new(1).unwrap(),
        &mut writer,
        |start, end, id, _| {
            let id = parse_general(id);
            if id != GeneralCategory::Unassigned {
                Some((start, end, id))
            } else {
                None
            }
        },
        |writer, mut values| {
            values.sort_unstable_by_key(|(start, _, _)| *start);
            write!(writer, "[").unwrap();
            for (start, end, id) in values {
                write!(writer, "({start}, {end}, GeneralCategory::{id:?}),").unwrap();
            }
            write!(writer, "];").unwrap();
        },
    );

    write_derived(
        &base,
        "DerivedBidiClass.txt",
        "BIDI_CLASS_LATEST",
        "(u32, u32, BidiClass)",
        NonZeroUsize::new(1).unwrap(),
        &mut writer,
        |start, end, id, _| {
            let id = parse_bidi(id);
            if id != "BidiClass::LeftToRight" {
                Some((start, end, id))
            } else {
                None
            }
        },
        |writer, mut values| {
            values.sort_unstable_by_key(|(start, _, _)| *start);
            write!(writer, "[").unwrap();
            for (start, end, id) in values {
                write!(writer, "({start}, {end}, {id}),").unwrap();
            }
            write!(writer, "];").unwrap();
        },
    );

    write_derived(
        &base,
        "DerivedCombiningClass.txt",
        "COMBINING_CLASS_LATEST",
        "(u32, u32, u8)",
        NonZeroUsize::new(1).unwrap(),
        &mut writer,
        |start, end, id, _| {
            let id: u8 = id.parse().unwrap();
            if id == 0 {
                return None;
            }
            Some((start, end, id))
        },
        |writer, mut values| {
            values.sort_unstable_by_key(|(start, _, _)| *start);
            write!(writer, "[").unwrap();
            for (start, end, id) in values {
                write!(writer, "({start}, {end}, {id}),").unwrap();
            }
            writeln!(writer, "];").unwrap();
        },
    );

    write_derived(
        &base,
        "EastAsianWidth.txt",
        "EAST_ASIAN_WIDTH_LATEST",
        "(u32, u32, EastAsianWidth)",
        NonZeroUsize::new(1).unwrap(),
        &mut writer,
        |start, end, id, _| {
            let id = parse_eaw(id);
            if id != "EastAsianWidth::Neutral" {
                Some((start, end, id))
            } else {
                None
            }
        },
        |writer, mut values| {
            values.sort_unstable_by_key(|(start, _, _)| *start);
            write!(writer, "[").unwrap();
            for (start, end, id) in values {
                write!(writer, "({start}, {end}, {id}),").unwrap();
            }
            write!(writer, "];").unwrap();
        },
    );

    write_derived(
        &base,
        "DerivedBinaryProperties.txt",
        "BIDI_MIRRORED_LATEST",
        "(u32, u32)",
        NonZeroUsize::new(1).unwrap(),
        &mut writer,
        |start, end, id, _| {
            assert_eq!(
                "Bidi_Mirrored",
                id.trim(),
                "DerivedBinaryProperties.txt only has Bidi_Mirrored"
            );
            Some((start, end))
        },
        |writer, mut values| {
            values.sort_unstable_by_key(|(start, _)| *start);
            writeln!(writer, "{values:?};").unwrap();
        },
    );

    write_derived(
        &base,
        "DerivedNumericType.txt",
        "NUMERIC_TYPE_LATEST",
        "(u32, u32, NumericType)",
        NonZeroUsize::new(1).unwrap(),
        &mut writer,
        |start, end, id, _| {
            let id = parse_numeric_type_str(id);
            if id != "NumericType::None" {
                Some((start, end, id))
            } else {
                None
            }
        },
        |writer, mut values| {
            values.sort_unstable_by_key(|(start, _, _)| *start);
            write!(writer, "[").unwrap();
            for (start, end, id) in values {
                write!(writer, "({start}, {end}, {id}),").unwrap();
            }
            writeln!(writer, "];").unwrap();
        },
    );

    // NOTE:
    // The decomposition table mirrors CPython's: canonical entries carry no
    // tag, compatibility entries carry the "<tag>". Hangul syllables are not
    // listed in UnicodeData.txt and are decomposed algorithmically in data.rs.
    let mut decomp_ranges = Vec::new();
    write_derived(
        &base,
        "UnicodeData.txt",
        "DECOMP",
        "(u32, DecompositionType, usize)",
        NonZeroUsize::new(5).unwrap(),
        &mut writer,
        |start, _end, value, _| {
            // We're building a sparse array. Most characters don't decompose, so we don't
            // need to literally store a row for each char.
            if value.is_empty() {
                return None;
            }

            let (dtype, decomp): (DecompositionType, Vec<u32>) = if value.starts_with('<') {
                let (dtype, decomp) = value.split_once('>').unwrap();
                (
                    parse_decomp_type(dtype.strip_prefix('<').unwrap()),
                    decomp
                        .split_whitespace()
                        .map(|s| u32::from_str_radix(s, 16).unwrap())
                        .collect(),
                )
            } else {
                (
                    DecompositionType::Canonical,
                    value
                        .split_whitespace()
                        .map(|s| u32::from_str_radix(s, 16).unwrap())
                        .collect(),
                )
            };

            decomp_ranges.extend(decomp);
            let end = decomp_ranges.len();

            Some((start, dtype, end))
        },
        |writer, values| {
            // UnicodeData.txt should already be sorted
            write!(writer, "[").unwrap();
            for (start, dtype, end) in values {
                write!(writer, "({start}, DecompositionType::{dtype:?}, {end}),").unwrap();
            }
            writeln!(writer, "];").unwrap();
        },
    );

    writeln!(writer, "static DECOMP_RANGE: &[u32] = &{decomp_ranges:?};").unwrap();

    // Names from UnicodeData.txt field 1. Algorithmic names (Hangul
    // syllables, CJK unified ideographs, Tangut) are not listed and are
    // computed in data.rs, mirroring CPython's derived_name_ranges.
    let mut names = Vec::new();
    let mut names_by_name = Vec::new();
    {
        let reader = BufReader::new(File::open(base.join("UnicodeData.txt")).unwrap());
        for line in reader.lines().map(Result::unwrap) {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split(';');
            let cp = u32::from_str_radix(fields.next().unwrap().trim(), 16).unwrap();
            let name = fields.next().unwrap().trim().to_owned();
            // CPython's makeunicodedata.py skips `<...>` names ("<control>",
            // "<private-use>", "<not a character>"), so name() raises
            // ValueError for them.
            if !name.is_empty() && !name.starts_with('<') {
                names.push((cp, name.clone()));
                names_by_name.push((name, cp));
            }
        }
    }
    names.sort_unstable_by_key(|&(cp, _)| cp);
    names_by_name.sort_by(|(a, _), (b, _)| a.cmp(b));

    write!(writer, "static NAMES: &[(u32, &str)] = &[").unwrap();
    for (cp, name) in &names {
        write!(writer, "(0x{cp:X}, {name:?}),").unwrap();
    }
    writeln!(writer, "];").unwrap();
    write!(writer, "static NAMES_BY_NAME: &[(&str, u32)] = &[").unwrap();
    for (name, cp) in &names_by_name {
        write!(writer, "({name:?}, 0x{cp:X}),").unwrap();
    }
    writeln!(writer, "];").unwrap();
}

#[expect(clippy::too_many_arguments)]
fn write_derived<W, P, FW, T>(
    base: &Path,
    file_name: &str,
    static_name: &str,
    array_type: &str,
    field: NonZeroUsize,
    writer: &mut W,
    parse: P,
    write_vec: FW,
) where
    W: Write,
    P: FnMut(u32, u32, &str, &str) -> Option<T>,
    FW: FnMut(&mut W, Vec<T>),
{
    let path = base.join(file_name);
    let reader = BufReader::new(File::open(path).unwrap());
    writeln!(writer, "static {static_name}: &[{array_type}] = &").unwrap();
    parse_unicode_3_2(reader, field, writer, parse, write_vec);
}

/// Parse Unicode 3.2.0 property files.
fn parse_unicode_3_2<W, P, FW, T>(
    reader: impl BufRead,
    field: NonZeroUsize,
    writer: &mut W,
    mut parse: P,
    mut write_vec: FW,
) where
    W: Write,
    P: FnMut(u32, u32, &str, &str) -> Option<T>,
    FW: FnMut(&mut W, Vec<T>),
{
    let mut parsed = Vec::new();

    for line in reader.lines().map(Result::unwrap) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let mut fields = line.split(';');
        let range = fields.next().expect("Unicode data is missing a char range");
        let id = fields
            .nth(field.get().saturating_sub(1))
            .expect("Unicode data is missing a property");
        let (start, end) = match range.split_once("..") {
            Some((left, right)) => {
                let start = u32::from_str_radix(left.trim(), 16).unwrap();
                let end = u32::from_str_radix(right.trim(), 16).unwrap();
                (start, end)
            }
            None => {
                let start = u32::from_str_radix(range.trim(), 16).unwrap();
                (start, start)
            }
        };

        let id = id.split_once('#').map_or(id, |(left, _)| left).trim();
        if let Some(val) = parse(start, end, id, line) {
            parsed.push(val);
        }
    }
    write_vec(writer, parsed);
}

fn parse_general(id: &str) -> GeneralCategory {
    match id.trim() {
        "Cn" => GeneralCategory::Unassigned,
        "Lu" => GeneralCategory::UppercaseLetter,
        "Ll" => GeneralCategory::LowercaseLetter,
        "Lt" => GeneralCategory::TitlecaseLetter,
        "Lm" => GeneralCategory::ModifierLetter,
        "Lo" => GeneralCategory::OtherLetter,
        "Mn" => GeneralCategory::NonspacingMark,
        "Mc" => GeneralCategory::SpacingMark,
        "Me" => GeneralCategory::EnclosingMark,
        "Nd" => GeneralCategory::DecimalNumber,
        "Nl" => GeneralCategory::LetterNumber,
        "No" => GeneralCategory::OtherNumber,
        "Zs" => GeneralCategory::SpaceSeparator,
        "Zl" => GeneralCategory::LineSeparator,
        "Zp" => GeneralCategory::ParagraphSeparator,
        "Cc" => GeneralCategory::Control,
        "Cf" => GeneralCategory::Format,
        "Co" => GeneralCategory::PrivateUse,
        "Cs" => GeneralCategory::Surrogate,
        "Pd" => GeneralCategory::DashPunctuation,
        "Ps" => GeneralCategory::OpenPunctuation,
        "Pe" => GeneralCategory::ClosePunctuation,
        "Pc" => GeneralCategory::ConnectorPunctuation,
        "Pi" => GeneralCategory::InitialPunctuation,
        "Pf" => GeneralCategory::FinalPunctuation,
        "Po" => GeneralCategory::OtherPunctuation,
        "Sm" => GeneralCategory::MathSymbol,
        "Sc" => GeneralCategory::CurrencySymbol,
        "Sk" => GeneralCategory::ModifierSymbol,
        "So" => GeneralCategory::OtherSymbol,
        invalid => unreachable!("Unicode data contains valid properties: {invalid}"),
    }
}

fn parse_eaw(id: &str) -> &'static str {
    match id.trim() {
        "N" => "EastAsianWidth::Neutral",
        "A" => "EastAsianWidth::Ambiguous",
        "H" => "EastAsianWidth::Halfwidth",
        "F" => "EastAsianWidth::Fullwidth",
        "Na" => "EastAsianWidth::Narrow",
        "W" => "EastAsianWidth::Wide",
        invalid => unreachable!("Unicode data contains valid properties: {invalid}"),
    }
}

fn parse_bidi(id: &str) -> &'static str {
    match id.trim() {
        "L" => "BidiClass::LeftToRight",
        "R" => "BidiClass::RightToLeft",
        "EN" => "BidiClass::EuropeanNumber",
        "ES" => "BidiClass::EuropeanSeparator",
        "ET" => "BidiClass::EuropeanTerminator",
        "AN" => "BidiClass::ArabicNumber",
        "CS" => "BidiClass::CommonSeparator",
        "B" => "BidiClass::ParagraphSeparator",
        "S" => "BidiClass::SegmentSeparator",
        "WS" => "BidiClass::WhiteSpace",
        "ON" => "BidiClass::OtherNeutral",
        "LRE" => "BidiClass::LeftToRightEmbedding",
        "LRO" => "BidiClass::LeftToRightOverride",
        "AL" => "BidiClass::ArabicLetter",
        "RLE" => "BidiClass::RightToLeftEmbedding",
        "RLO" => "BidiClass::RightToLeftOverride",
        "PDF" => "BidiClass::PopDirectionalFormat",
        "NSM" => "BidiClass::NonspacingMark",
        "BN" => "BidiClass::BoundaryNeutral",
        "FSI" => "BidiClass::FirstStrongIsolate",
        "LRI" => "BidiClass::LeftToRightIsolate",
        "RLI" => "BidiClass::RightToLeftIsolate",
        "PDI" => "BidiClass::PopDirectionalIsolate",
        invalid => unreachable!("Unicode data contains valid properties: {invalid}"),
    }
}

fn parse_numeric_type_str(id: &str) -> &'static str {
    match id.trim().to_ascii_lowercase().as_str() {
        "none" => "NumericType::None",
        "decimal" => "NumericType::Decimal",
        "digit" => "NumericType::Digit",
        "numeric" => "NumericType::Numeric",
        invalid => unreachable!("Unicode data contains valid properties: {invalid}"),
    }
}

#[derive(Debug, Default)]
enum DecompositionType {
    #[default]
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

fn parse_decomp_type(id: &str) -> DecompositionType {
    match id {
        "canonical" => DecompositionType::Canonical,
        "compat" => DecompositionType::Compat,
        "circle" => DecompositionType::Circle,
        "final" => DecompositionType::Final,
        "font" => DecompositionType::Font,
        "fraction" => DecompositionType::Fraction,
        "initial" => DecompositionType::Initial,
        "isolated" => DecompositionType::Isolated,
        "medial" => DecompositionType::Medial,
        "narrow" => DecompositionType::Narrow,
        "noBreak" => DecompositionType::Nobreak,
        "small" => DecompositionType::Small,
        "square" => DecompositionType::Square,
        "sub" => DecompositionType::Sub,
        "super" => DecompositionType::Super,
        "vertical" => DecompositionType::Vertical,
        "wide" => DecompositionType::Wide,
        invalid => unreachable!("Unicode data contains valid properties: {invalid}"),
    }
}

fn main() {
    println!("cargo:rerun-if-changed=unicode/ucd32");
    println!("cargo:rerun-if-changed=unicode/latest");

    let t_32 = thread::spawn(generate_unicode_3_2);
    let t_numeric_type = thread::spawn(generate_numeric_type);
    let t_numeric_value = thread::spawn(generate_numeric_value);
    let t_latest = thread::spawn(generate_unicode_latest);
    t_32.join().unwrap();
    t_numeric_type.join().unwrap();
    t_numeric_value.join().unwrap();
    t_latest.join().unwrap();
}
