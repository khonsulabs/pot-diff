use pot::{OwnedValue, Value};
use serde::{Deserialize, Serialize};

use crate::Diff;

#[track_caller]
fn test<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(
    original: &T,
    updated: &T,
    diff_display: &str,
) {
    let diff = Diff::between(original, updated);
    println!("Updating {original:?} to {updated:?} using {diff}");
    assert_eq!(diff.to_string(), diff_display);

    let applied = diff.apply(original).unwrap();
    assert_eq!(&applied, updated);

    let mut encoded = Vec::new();
    crate::binary::encode(&diff, &mut encoded).unwrap();
    println!("Encoded to {} bytes: {:?}", encoded.len(), encoded);
    let decoded = crate::binary::decode(&encoded).unwrap();
    let applied = decoded.apply(original).unwrap();
    assert_eq!(&applied, updated);
}

#[test]
fn basic_sequence_apply() {
    test(&vec![1], &vec![1], "");
    test(&vec![1, 2], &vec![1, 2, 3], "[;+2;3]");
    test(&vec![0, 1, 2, 3], &vec![2, 3], "[;-0;2]");
    test(&vec![3, 4], &vec![1, 2, 3, 4], "[;+0;1+1;2]");
    test(&vec![3, 4, 5], &vec![3, 4], "[;$2]");

    test(
        &vec![vec![1, 2], vec![2, 3, 4]],
        &vec![vec![1, 2, 3], vec![2, 3, 4]],
        "[;[0;+2;3]]",
    );
    test(
        &vec![vec![1], vec![2, 3, 4]],
        &vec![vec![0], vec![2, 3, 4]],
        "[;~0;[0]]",
    );
}

#[test]
fn map_sequence_apply() {
    test(
        &OwnedValue(Value::from_mappings([(Value::from(1), Value::from(2))])),
        &OwnedValue(Value::from_mappings([
            (Value::from(1), Value::from(2)),
            (Value::from(3), Value::from(4)),
        ])),
        "{;+1;3;4}",
    );
    test(
        &OwnedValue(Value::from_mappings([(Value::from(1), Value::from(2))])),
        &OwnedValue(Value::from_mappings([(Value::from(1), Value::from(3))])),
        "{;~0;3}",
    );
    test(
        &OwnedValue(Value::from_mappings([(
            Value::from(1),
            Value::from_sequence([Value::from(1), Value::from(3)]),
        )])),
        &OwnedValue(Value::from_mappings([(
            Value::from(1),
            Value::from_sequence([Value::from(2), Value::from(3)]),
        )])),
        "{;[0;~0;2]}",
    );
    test(
        &OwnedValue(Value::from_mappings([
            (Value::from(1), Value::from(2)),
            (Value::from(3), Value::from(4)),
        ])),
        &OwnedValue(Value::from_mappings([(Value::from(3), Value::from(4))])),
        "{;-0;1}",
    );
    test(
        &OwnedValue(Value::from_mappings([(Value::from(3), Value::from(4))])),
        &OwnedValue(Value::from_mappings([
            (Value::from(1), Value::from(2)),
            (Value::from(3), Value::from(4)),
        ])),
        "{;+0;1;2}",
    );
    test(
        &OwnedValue(Value::from_mappings([
            (Value::from(1), Value::from(2)),
            (Value::from(3), Value::from(4)),
        ])),
        &OwnedValue(Value::from_mappings([(Value::from(1), Value::from(2))])),
        "{;$1}",
    );
    test(
        &OwnedValue(Value::from_mappings([
            (Value::from(1), Value::from(2)),
            (Value::from(4), Value::from(5)),
        ])),
        &OwnedValue(Value::from_mappings([
            (Value::from(3), Value::from(4)),
            (Value::from(4), Value::from(5)),
        ])),
        "{;~0;3;4}",
    );
    test(
        &OwnedValue(Value::from_mappings([(Value::from(1), Value::from(2))])),
        &OwnedValue(Value::from_mappings([(Value::from(3), Value::from(4))])),
        "~;{3:4}",
    );
    test(
        &OwnedValue(Value::from_mappings([(
            Value::from_sequence([Value::from(1), Value::from(3), Value::from(4)]),
            Value::from_sequence([Value::from(3), Value::from(4)]),
        )])),
        &OwnedValue(Value::from_mappings([(
            Value::from_sequence([Value::from(2), Value::from(3), Value::from(4)]),
            Value::from_sequence([Value::from(3), Value::from(4)]),
        )])),
        "{;[@0;~0;2]}",
    );
    test(
        &OwnedValue(Value::from_mappings([(
            Value::from_sequence([Value::from(1), Value::from(3), Value::from(4)]),
            Value::from_sequence([Value::from(3), Value::from(4)]),
        )])),
        &OwnedValue(Value::from_mappings([(
            Value::from_sequence([Value::from(2)]),
            Value::from_sequence([Value::from(3), Value::from(4)]),
        )])),
        "{;~@0;[2]}",
    );
}

#[test]
fn root_operations() {
    test(&1, &2, "~;2");
    test(
        &OwnedValue(Value::Bool(true)),
        &OwnedValue(Value::None),
        "~;none",
    );
    test(
        &OwnedValue(Value::Bool(true)),
        &OwnedValue(Value::Bool(false)),
        "~;false",
    );
    test(
        &OwnedValue(Value::from(0.)),
        &OwnedValue(Value::from(1.)),
        "~;1",
    );
    test(
        &OwnedValue(Value::from(b"hello")),
        &OwnedValue(Value::from(b"worl\xdd")),
        "~;#|worl|dd",
    );
    test(
        &OwnedValue(Value::from("hello")),
        &OwnedValue(Value::from("world")),
        "~;\"world\"",
    );

    // replace instead of update
    test(&vec![0, 1, 2, 3, 4, 5, 6, 7], &vec![1, 7], "~;[1,7]")
}
