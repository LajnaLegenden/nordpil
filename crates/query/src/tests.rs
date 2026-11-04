use bson::oid::ObjectId;
use bson::spec::BinarySubtype;
use bson::{Binary, Bson, DateTime, Decimal128, Document, Regex, Timestamp, doc};
use proptest::prelude::*;

use super::*;

fn regex(pattern: &str, options: &str) -> Bson {
    Bson::RegularExpression(Regex {
        pattern: pattern.to_owned().try_into().unwrap(),
        options: options.to_owned().try_into().unwrap(),
    })
}

#[test]
fn parses_shell_syntax() {
    let parsed = parse_document(
        r#"{
            _id: ObjectId('65a1b2c3d4e5f60718293a4b'),
            small: 5, big: 3000000000, huge: 1e30, half: .5, neg: -2147483648, hex: 0xff,
            "quoted key": "x", 'it\'s': 'y',
            when: ISODate('2024-01-31'), at: new Date('2024-01-31T12:30:00+02:00'), ms: new Date(0),
            long: NumberLong('9007199254740993'), int: NumberInt(-7), dec: NumberDecimal('1.10'),
            re: /^a\/b/xi, re2: RegExp('c', 'm'),
            uuid: UUID('0e8c5f3a-1b2c-4d5e-8f90-a1b2c3d4e5f6'), bin: BinData(0, 'aGk='),
            ts: Timestamp({ t: 1, i: 2 }), ts2: Timestamp(3, 4),
            flags: [true, false, null], keys: [MinKey(), MaxKey],
            nested: { $in: [1, 'two',], },
        }"#,
    )
    .unwrap();

    let expected = doc! {
        "_id": ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap(),
        "small": 5_i32, "big": 3_000_000_000_i64, "huge": 1e30, "half": 0.5,
        "neg": i32::MIN, "hex": 255_i32,
        "quoted key": "x", "it's": "y",
        "when": DateTime::parse_rfc3339_str("2024-01-31T00:00:00Z").unwrap(),
        "at": DateTime::parse_rfc3339_str("2024-01-31T10:30:00Z").unwrap(),
        "ms": DateTime::from_millis(0),
        "long": 9_007_199_254_740_993_i64, "int": -7_i32,
        "dec": "1.10".parse::<Decimal128>().unwrap(),
        "re": regex("^a/b", "ix"), "re2": regex("c", "m"),
        "uuid": Binary::from_uuid(bson::uuid::Uuid::parse_str("0e8c5f3a-1b2c-4d5e-8f90-a1b2c3d4e5f6").unwrap()),
        "bin": Binary { subtype: BinarySubtype::Generic, bytes: b"hi".to_vec() },
        "ts": Timestamp { time: 1, increment: 2 }, "ts2": Timestamp { time: 3, increment: 4 },
        "flags": [true, false, Bson::Null], "keys": [Bson::MinKey, Bson::MaxKey],
        "nested": { "$in": [1_i32, "two"] },
    };
    assert_eq!(parsed, expected);
}

#[test]
fn empty_filter_is_empty_document() {
    assert_eq!(parse_document("  "), Ok(Document::new()));
}

#[test]
fn explains_bad_values() {
    let messages = [
        "{ a: b }",
        "{ a: Foo(1) }",
        "{ a: ObjectId('123') }",
        "{ a: /x/g }",
        "{ a: NumberInt('1.5') }",
        "{ a: ISODate('yesterday') }",
        "{ a: -'x' }",
        "{ a: undefined }",
        "[1, 2]",
    ]
    .map(|text| {
        let errors = parse_document(text).unwrap_err();
        (
            text,
            errors.into_iter().map(|e| e.message).collect::<Vec<_>>(),
        )
    });
    insta::assert_debug_snapshot!(messages);
}

#[test]
fn formats_compact_and_pretty() {
    let document = doc! {
        "name": "Ada", "$gt": 1.0, "a.b": [1_i32, 2_i64], "empty": {}, "list": [],
        "when": DateTime::from_millis(1_700_000_000_123),
        "re": regex("a/b", "i"),
    };
    insta::assert_snapshot!(format_document(&document, Style::Compact));
    insta::assert_snapshot!(format_document(&document, Style::Pretty(2)));
}

fn leaf() -> impl Strategy<Value = Bson> {
    prop_oneof![
        any::<i32>().prop_map(Bson::Int32),
        any::<i64>().prop_map(Bson::Int64),
        any::<f64>().prop_map(Bson::Double),
        prop_oneof![Just(f64::NAN), Just(f64::INFINITY), Just(-0.0)].prop_map(Bson::Double),
        any::<String>().prop_map(Bson::String),
        any::<bool>().prop_map(Bson::Boolean),
        Just(Bson::Null),
        any::<[u8; 12]>().prop_map(|bytes| Bson::ObjectId(ObjectId::from_bytes(bytes))),
        any::<i64>().prop_map(|ms| Bson::DateTime(DateTime::from_millis(ms))),
        (-62_135_596_800_000_i64..253_402_300_799_999)
            .prop_map(|ms| Bson::DateTime(DateTime::from_millis(ms))),
        (any::<u8>(), proptest::collection::vec(any::<u8>(), 0..20)).prop_map(
            |(subtype, bytes)| {
                Bson::Binary(Binary {
                    subtype: BinarySubtype::from(subtype),
                    bytes,
                })
            }
        ),
        any::<[u8; 16]>()
            .prop_map(|bytes| Bson::Binary(Binary::from_uuid(bson::uuid::Uuid::from_bytes(bytes)))),
        (any::<u32>(), any::<u32>())
            .prop_map(|(time, increment)| Bson::Timestamp(Timestamp { time, increment })),
        (
            "[a-z/\\\\\\[\\]\\n. ]{0,8}",
            proptest::sample::subsequence(vec!['i', 'l', 'm', 's', 'u', 'x'], 0..3)
        )
            .prop_map(|(pattern, flags)| regex(&pattern, &flags.into_iter().collect::<String>())),
        "-?[0-9]{1,20}(\\.[0-9]{1,10})?(E[+-]?[0-9]{1,3})?"
            .prop_map(|text| Bson::Decimal128(text.parse().unwrap())),
        Just(Bson::MinKey),
        Just(Bson::MaxKey),
        any::<String>().prop_map(Bson::JavaScriptCode),
    ]
}

fn value() -> impl Strategy<Value = Bson> {
    leaf().prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(Bson::Array),
            proptest::collection::vec(("[^\\x00]{0,6}", inner), 0..4)
                .prop_map(|fields| Bson::Document(fields.into_iter().collect())),
        ]
    })
}

proptest! {
    /// Formatting then parsing gives the same value. Compared through the
    /// formatted text, since NaN != NaN.
    #[test]
    fn format_then_parse_round_trips(value in value(), pretty in any::<bool>()) {
        let style = if pretty { Style::Pretty(2) } else { Style::Compact };
        let text = format_value(&value, style);
        let parsed = parse_value(&text).map_err(|errors| {
            TestCaseError::fail(format!("{text:?} did not parse: {errors:?}"))
        })?;
        prop_assert_eq!(format_value(&parsed, Style::Compact), format_value(&value, Style::Compact));
    }

    /// Any input, however broken, parses without panicking.
    #[test]
    fn never_panics(text in "\\PC{0,40}") {
        let _ = parse_value(&text);
        let _ = highlight(&text);
    }

    /// Pipeline completion and splitting never panic, at any cursor.
    #[test]
    fn pipeline_tools_never_panic(text in "([a-z.(){}\\[\\]'\"/\n $:,]|\\$group|\\$lookup|\\$match|pipeline|from|\\PC){0,50}") {
        let _ = crate::pipeline::split_pipeline(&text);
        let collections = ["users".to_owned()];
        let hints = crate::Hints { collections: &collections, ..crate::Hints::default() };
        let modes = [
            crate::Mode::Pipeline,
            crate::Mode::Stage(crate::pipeline::stage("$group").unwrap()),
            crate::Mode::Stage(crate::pipeline::stage("$lookup").unwrap()),
            crate::Mode::Stage(crate::pipeline::stage("$unwind").unwrap()),
        ];
        for (cursor, _) in text.char_indices().chain([(text.len(), ' ')]) {
            for mode in modes {
                let _ = crate::complete_with(&text, cursor, mode, &hints, true);
            }
        }
    }

    /// Statements of any script are in order, apart, inside the text and on
    /// char boundaries; highlighting and completion never panic.
    #[test]
    fn script_tools_never_panic(text in "([a-z.;(){}\\[\\]'\"`/=+\n $:,]|db|if|do|else|use|const|\\PC){0,60}") {
        let statements = split_statements(&text);
        let mut end = 0;
        for statement in &statements {
            prop_assert!(statement.span.start >= end);
            prop_assert!(statement.span.start < statement.span.end);
            prop_assert!(text.get(statement.span.clone()).is_some());
            end = statement.span.end;
        }
        let _ = highlight_script(&text);
        let collections = ["users".to_owned()];
        let hints = ScriptHints { collections: &collections, ..ScriptHints::default() };
        for (cursor, _) in text.char_indices().chain([(text.len(), ' ')]) {
            let _ = statement_at(&text, &statements, cursor);
            let _ = complete_script(&text, cursor, &hints, true);
            let _ = script_target(&text, cursor);
        }
    }
}

#[test]
fn preview_cuts_long_documents_on_a_char_boundary() {
    let document = doc! { "text": "å".repeat(1000) };

    let preview = preview(&document, 101);

    assert!(preview.text.len() <= 101);
    assert!(preview.text.is_char_boundary(preview.text.len()));
    assert_eq!(
        preview.full_len,
        format_document(&document, Style::Pretty(2)).len()
    );
    assert!(
        preview
            .spans
            .iter()
            .all(|(span, _)| span.end <= preview.text.len())
    );
}
