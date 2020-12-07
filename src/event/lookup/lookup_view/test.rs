use crate::event::lookup::*;
use std::{fs, io::Read, path::Path};

const SUFFICIENTLY_COMPLEX: &str =
    r#"regular."quoted"."quoted but spaces"."quoted.but.periods".lookup[0].nested_lookup[0][0]"#;
lazy_static::lazy_static! {
    static ref SUFFICIENTLY_DECOMPOSED: [Segment<'static>; 9] = [
        Segment::field(r#"regular"#),
        Segment::field(r#""quoted""#),
        Segment::field(r#""quoted but spaces""#),
        Segment::field(r#""quoted.but.periods""#),
        Segment::field(r#"lookup"#),
        Segment::index(0),
        Segment::field(r#"nested_lookup"#),
        Segment::index(0),
        Segment::index(0),
    ];
}

#[test]
fn zero_len_not_allowed() {
    crate::test_util::trace_init();
    let input = "";
    let maybe_lookup = Lookup::from_str(input);
    assert!(maybe_lookup.is_err());
}

#[test]
fn we_dont_parse_plain_strings_in_from() {
    crate::test_util::trace_init();
    let input = "some_key.still_the_same_key.this.is.going.in.via.from.and.should.not.get.parsed";
    let lookup = Lookup::from(input);
    assert_eq!(lookup[0], Segment::field(input));
    assert_eq!(lookup.to_string(), input);
}

#[test]
fn simple() {
    crate::test_util::trace_init();
    let input = "some_key";
    let lookup = Lookup::from_str(input).unwrap();
    assert_eq!(lookup[0], Segment::field("some_key"));
    assert_eq!(lookup.to_string(), input);
}

#[test]
fn push() {
    crate::test_util::trace_init();
    let input = "some_key";
    let mut lookup = Lookup::from_str(input).unwrap();
    lookup.push(Segment::field(input));
    assert_eq!(lookup[0], Segment::from("some_key"));
    assert_eq!(lookup[1], Segment::from("some_key"));
}

#[test]
fn pop() {
    crate::test_util::trace_init();
    let input = "some_key";
    let mut lookup = Lookup::from_str(input).unwrap();
    let out = lookup.pop();
    assert_eq!(out, Some(Segment::field("some_key")));
}

#[test]
fn array() {
    crate::test_util::trace_init();
    let input = "foo[0]";
    let lookup = Lookup::from_str(input).unwrap();
    assert_eq!(lookup[0], Segment::field("foo"));
    assert_eq!(lookup[1], Segment::index(0));
    assert_eq!(lookup.to_string(), input);
}

#[test]
fn fields() {
    crate::test_util::trace_init();
    let input = "florp.flop";
    let lookup = Lookup::from_str(input).unwrap();
    assert_eq!(lookup[0], Segment::field("florp"));
    assert_eq!(lookup[1], Segment::field("flop"));
    assert_eq!(lookup.to_string(), input);
}

#[test]
fn fields_with_quotes() {
    crate::test_util::trace_init();
    let input = "florp.\"flop fleep\"";
    let lookup = Lookup::from_str(input).unwrap();
    assert_eq!(lookup[0], Segment::field("florp"));
    assert_eq!(lookup[1], Segment::field(r#""flop fleep""#));
    assert_eq!(lookup.to_string(), input);
}

#[test]
fn to_string() {
    crate::test_util::trace_init();
    let input = SUFFICIENTLY_COMPLEX;
    let lookup = Lookup::from_str(input).unwrap();
    assert_eq!(lookup.to_string(), input);
}

#[test]
fn impl_index_ranges() {
    crate::test_util::trace_init();
    let lookup = Lookup::from_str(SUFFICIENTLY_COMPLEX).unwrap();

    // This test is primarily to ensure certain interfaces exist and weren't disturbed.
    assert_eq!(lookup[..], SUFFICIENTLY_DECOMPOSED[..]);
    assert_eq!(lookup[..4], SUFFICIENTLY_DECOMPOSED[..4]);
    assert_eq!(lookup[..=4], SUFFICIENTLY_DECOMPOSED[..=4]);
    assert_eq!(lookup[2..], SUFFICIENTLY_DECOMPOSED[2..]);
}

#[test]
fn impl_index_usize() {
    crate::test_util::trace_init();
    let lookup = Lookup::from_str(SUFFICIENTLY_COMPLEX).unwrap();

    for i in 0..SUFFICIENTLY_DECOMPOSED.len() {
        assert_eq!(lookup[i], SUFFICIENTLY_DECOMPOSED[i])
    }
}

#[test]
fn impl_index_mut_index_mut() {
    crate::test_util::trace_init();
    let mut lookup = Lookup::from_str(SUFFICIENTLY_COMPLEX).unwrap();

    for i in 0..SUFFICIENTLY_DECOMPOSED.len() {
        let x = &mut lookup[i]; // Make sure we force a mutable borrow!
        assert_eq!(*x, SUFFICIENTLY_DECOMPOSED[i])
    }
}

#[test]
fn iter() {
    crate::test_util::trace_init();
    let lookup = Lookup::from_str(SUFFICIENTLY_COMPLEX).unwrap();

    let mut iter = lookup.iter();
    for (index, expected) in SUFFICIENTLY_DECOMPOSED.iter().enumerate() {
        let parsed = iter
            .next()
            .unwrap_or_else(|| panic!("Expected at index {}: {:?}, got None.", index, expected));
        assert_eq!(expected, parsed, "Failed at {}", index);
    }
}

#[test]
fn into_iter() {
    crate::test_util::trace_init();
    let lookup = Lookup::from_str(SUFFICIENTLY_COMPLEX).unwrap();
    let mut iter = lookup.into_iter();
    for (index, expected) in SUFFICIENTLY_DECOMPOSED.iter().cloned().enumerate() {
        let parsed = iter
            .next()
            .unwrap_or_else(|| panic!("Expected at index {}: {:?}, got None.", index, expected));
        assert_eq!(expected, parsed, "Failed at {}", index);
    }
}

fn parse_artifact(path: impl AsRef<Path>) -> std::io::Result<String> {
    let mut test_file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) => return Err(e),
    };

    let mut buf = Vec::new();
    test_file.read_to_end(&mut buf)?;
    let string = String::from_utf8(buf).unwrap();
    // remove trailing newline introduced by editors
    Ok(string.trim_end().to_owned())
}

// This test iterates over the `tests/data/fixtures/lookup` folder and ensures the lookup parsed,
// then turned into a string again is the same.
#[test]
fn lookup_to_string_and_serialize() {
    crate::test_util::trace_init();
    const FIXTURE_ROOT: &str = "tests/data/fixtures/lookup";

    trace!(?FIXTURE_ROOT, "Opening.");
    std::fs::read_dir(FIXTURE_ROOT)
        .unwrap()
        .for_each(|fixture_file| match fixture_file {
            Ok(fixture_file) => {
                let path = fixture_file.path();
                tracing::trace!(?path, "Opening.");
                let buf = parse_artifact(&path).unwrap();

                let buf_serialized =
                    serde_json::to_string(&serde_json::to_value(&buf).unwrap()).unwrap();
                let lookup = Lookup::from_str(&buf).unwrap();
                tracing::trace!(?path, ?lookup, ?buf, "Asserting equal.");
                assert_eq!(lookup.to_string(), buf);

                // **WARNING:**: You **can not** deserialize lookups (that is, views, the buffers
                // are fine) out of str slices with escapes. It's invalid. You **must** use lookupbufs.
                if lookup.to_string().contains('\"') {
                    trace!("Need to filter this test, it contains escape chars and cannot be \
                            deserialized into a lookup view. Use a LookupBuf.");
                    return;
                }

                // Ensure serialization doesn't clobber.
                let serialized = serde_json::to_string(&lookup.to_string()).unwrap();
                assert_eq!(serialized, buf_serialized);
                // Ensure deserializing doesn't clobber.
                let deserialized: Lookup<'_> = serde_json::from_str(&serialized).unwrap();
                assert_eq!(lookup, deserialized);
            }
            _ => panic!("This test should never read Err'ing test fixtures."),
        });
}
