use std::borrow::Cow;
use std::collections::{vec_deque, VecDeque};
use std::fmt::{Display, Write as _};
use std::iter::{self, Cloned};
use std::ops::{Deref, DerefMut};
use std::slice;

use pot::format::{Float, Integer};
use pot::Value;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::text::ValueDisplay;

mod binary;
mod text;

#[derive(Debug, PartialEq)]
pub struct Diff {
    changes: Vec<Change>,
}

impl Diff {
    pub fn serialize(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        binary::encode(self, &mut bytes).expect("infallible");
        bytes
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, binary::DecodeError> {
        binary::decode(bytes)
    }

    pub fn between<T: Serialize>(original: &T, updated: &T) -> Self {
        let original = Value::from_serialize(original);
        let updated = Value::from_serialize(updated);
        Self::between_values(&original, updated)
    }

    pub fn between_values(original: &Value<'_>, updated: Value<'static>) -> Self {
        let mut diff = Self {
            changes: Vec::new(),
        };

        let updated = Estimated::from(updated);

        // We want to figure out if we should replace this value or
        // generate a diff for the value.
        let mut stats = Counter::default();
        Self::create_diff(None, original, Cow::Borrowed(&updated), false, &mut stats);
        if stats.estimated_bytes > updated.estimated_bytes {
            // Just replace the value rather than creating a diff.
            diff.log_change(updated.estimated_bytes, || Change::Replace {
                index: None,
                value: updated.value.into(),
            })
        } else {
            Self::create_diff(None, original, Cow::Owned(updated), false, &mut diff);
        }

        // Remove trailing exits, they're unnecessary
        while let Some(Change::Exit) = diff.changes.last() {
            diff.changes.pop();
        }

        diff
    }

    fn create_diff<D>(
        diff_index: Option<usize>,
        original: &Value<'_>,
        updated: Cow<'_, Estimated>,
        is_key: bool,
        diff: &mut D,
    ) where
        D: Differ,
    {
        match (original, &updated.value) {
            (Value::None, EstimatedValue::None) | (Value::Unit, EstimatedValue::Unit) => {}
            (Value::Bool(original), EstimatedValue::Bool(updated)) if original == updated => {}
            (Value::Integer(original), EstimatedValue::Integer(updated)) if original == updated => {
            }
            (Value::Float(original), EstimatedValue::Float(updated)) if original == updated => {}
            (Value::Bytes(original), EstimatedValue::Bytes(updated)) if original == updated => {}
            (Value::String(original), EstimatedValue::String(updated)) if original == updated => {}
            (Value::Sequence(original), EstimatedValue::Sequence(updated_sequence)) => {
                if updated_sequence != original {
                    diff.log_change(estimate_usize_bytes(diff_index.unwrap_or(0)), || Change::EnterSequence{ index: diff_index, key: is_key });
                    Self::create_sequence_diff(
                        original,
                        match updated {
                            Cow::Owned(Estimated {
                                value: EstimatedValue::Sequence(deque),
                                ..
                            }) => CowDeque::Owned(deque),
                            Cow::Borrowed(_) => CowDeque::Borrowed {
                                deque: updated_sequence,
                                index: 0,
                            },
                            Cow::Owned(_) => unreachable!(),
                        },
                        diff,
                    );
                    diff.log_change(0, || Change::Exit);
                }
            }
            (Value::Mappings(original), EstimatedValue::Mappings(updated_mappings)) => {
                if original.len() != updated_mappings.len() || updated_mappings
                    .iter()
                    .zip(original.iter())
                    .any(|(a, b)| a.0 != b.0 || a.1 != b.1)
                {
                    diff.log_change(diff_index.unwrap_or(0), || Change::EnterMap{ index: diff_index, key: is_key });
                    Self::create_map_diff(original, match updated {
                        Cow::Owned(Estimated {
                            value: EstimatedValue::Mappings(deque),
                            ..
                        }) => CowDeque::Owned(deque),
                        Cow::Borrowed(_) => CowDeque::Borrowed {
                            deque: updated_mappings,
                            index: 0,
                        },
                        Cow::Owned(_) => unreachable!(),
                    }, diff);
                    diff.log_change(0, || Change::Exit);
                }
            }
            _ => diff.log_change(updated.estimated_bytes, || {
                unreachable!("replace should happen after measurement due to log_change always adding 1 to estimated_bytes")
            }),
        }
    }

    fn create_sequence_diff<D>(
        original_values: &[Value<'_>],
        mut updated_values: CowDeque<'_, Estimated>,
        diff: &mut D,
    ) where
        D: Differ,
    {
        let mut original_index = 0;
        let mut insert_index = 0;

        while let Some(updated) = updated_values.pop_front() {
            if let Some(original) = original_values.get(original_index) {
                if let Some(matching_index) = original_values[original_index..]
                    .iter()
                    .enumerate()
                    .find_map(|(index, o)| (updated.as_ref() == o).then_some(index))
                {
                    // We found where the the updated value is located in the
                    // original list.
                    if matching_index > 0 {
                        diff.log_change(
                            estimate_usize_bytes(insert_index)
                                + estimate_usize_bytes(matching_index),
                            || Change::Remove {
                                index: insert_index,
                                length: matching_index,
                            },
                        );
                        original_index += matching_index;
                    }

                    // Skip the match
                    original_index += 1;
                    insert_index += 1;
                } else if let Some(matching_index) = updated_values
                    .iter()
                    .enumerate()
                    .find_map(|(index, updated)| (updated == original).then_some(index))
                {
                    // We found where the the original value is located in the
                    // updated list.
                    let mut updated = updated;
                    for _ in 0..matching_index + 1 {
                        diff.log_change(
                            updated.estimated_bytes + estimate_usize_bytes(insert_index),
                            || Change::Insert {
                                index: insert_index,
                                value: updated.into_owned().into(),
                            },
                        );
                        insert_index += 1;
                        updated = updated_values.pop_front().expect("just iterated");
                    }

                    // Skip the match
                    original_index += 1;
                    insert_index += 1;
                } else {
                    // We want to figure out if we should replace this value or
                    // generate a diff for the value.
                    let mut stats = Counter::default();
                    Self::create_diff(
                        Some(insert_index),
                        original,
                        Cow::Borrowed(&updated),
                        false,
                        &mut stats,
                    );
                    if stats.estimated_bytes > updated.estimated_bytes {
                        // Just replace the value rather than creating a diff.
                        diff.log_change(
                            updated.estimated_bytes + estimate_usize_bytes(insert_index),
                            || Change::Replace {
                                index: Some(insert_index),
                                value: updated.into_owned().into(),
                            },
                        )
                    } else {
                        Self::create_diff(Some(insert_index), original, updated, false, diff);
                    }
                    original_index += 1;
                    insert_index += 1;
                }
            } else {
                // Pushing a new value
                diff.log_change(
                    updated.estimated_bytes + estimate_usize_bytes(insert_index),
                    || Change::Insert {
                        index: insert_index,
                        value: updated.into_owned().into(),
                    },
                );
                insert_index += 1;
            }
        }

        if original_index < original_values.len() {
            // Extra values, need to truncate.
            diff.log_change(estimate_usize_bytes(insert_index), || Change::Truncate {
                length: insert_index,
            });
        }
    }

    fn create_map_diff<D>(
        original_values: &[(Value<'_>, Value<'_>)],
        mut updated_values: CowDeque<'_, (Estimated, Estimated)>,
        diff: &mut D,
    ) where
        D: Differ,
    {
        let mut original_index = 0;
        let mut insert_index = 0;

        while let Some(updated) = updated_values.pop_front() {
            if let Some(original) = original_values.get(original_index) {
                if let Some(matching_index) = original_values[original_index..]
                    .iter()
                    .enumerate()
                    .find_map(|(index, o)| (updated.0 == o.0).then_some(index))
                {
                    // We found where the the updated value is located in the
                    // original list.
                    if matching_index > 0 {
                        diff.log_change(
                            estimate_usize_bytes(insert_index) * matching_index,
                            || Change::Remove {
                                index: insert_index,
                                length: matching_index,
                            },
                        );
                        original_index += matching_index;
                    }

                    if updated.1 != original_values[original_index].1 {
                        // The value for this key has changed.
                        let mut stats = Counter::default();
                        Self::create_diff(
                            Some(insert_index),
                            &original_values[original_index].1,
                            Cow::Borrowed(&updated.1),
                            false,
                            &mut stats,
                        );
                        if stats.estimated_bytes > updated.1.estimated_bytes {
                            diff.log_change(
                                updated.1.estimated_bytes + estimate_usize_bytes(insert_index),
                                || Change::Replace {
                                    index: Some(insert_index),
                                    value: updated.into_owned().1.into(),
                                },
                            );
                        } else {
                            Self::create_diff(
                                Some(insert_index),
                                &original_values[original_index].1,
                                Cow::Borrowed(&updated.1),
                                false,
                                diff,
                            );
                        }
                    }

                    // Skip the match
                    original_index += 1;
                    insert_index += 1;
                } else if let Some(matching_index) = updated_values
                    .iter()
                    .enumerate()
                    .find_map(|(index, updated)| (updated.0 == original.0).then_some(index))
                {
                    // We found where the the original value is located in the
                    // updated list.
                    let mut updated_entry = updated;
                    for _ in 0..matching_index + 1 {
                        diff.log_change(
                            updated_entry.0.estimated_bytes
                                + updated_entry.1.estimated_bytes
                                + estimate_usize_bytes(insert_index),
                            || {
                                let owned = updated_entry.into_owned();
                                Change::InsertMapping {
                                    index: insert_index,
                                    key: owned.0.into(),
                                    value: owned.1.into(),
                                }
                            },
                        );
                        insert_index += 1;
                        updated_entry = updated_values.pop_front().expect("just iterated");
                    }

                    // Skip the match
                    original_index += 1;
                    insert_index += 1;
                } else {
                    if updated.1 == original.1 {
                        // This contains only a change to the key.
                        let mut stats = Counter::default();
                        // TODO we need to include Enter/Exit
                        Self::create_diff(
                            Some(insert_index),
                            &original.0,
                            Cow::Borrowed(&updated.0),
                            true,
                            &mut stats,
                        );
                        if stats.estimated_bytes > updated.0.estimated_bytes {
                            diff.log_change(
                                updated.0.estimated_bytes + estimate_usize_bytes(insert_index),
                                || {
                                    let owned = updated.into_owned();
                                    Change::ReplaceKey {
                                        index: insert_index,
                                        key: owned.0.into(),
                                    }
                                },
                            );
                        } else {
                            Self::create_diff(
                                Some(insert_index),
                                &original.0,
                                Cow::Borrowed(&updated.0),
                                true,
                                diff,
                            );
                        }
                    } else {
                        // Replace the entire entry
                        diff.log_change(
                            updated.0.estimated_bytes
                                + updated.1.estimated_bytes
                                + estimate_usize_bytes(insert_index),
                            || {
                                let owned = updated.into_owned();
                                Change::ReplaceMapping {
                                    index: insert_index,
                                    key: owned.0.into(),
                                    value: owned.1.into(),
                                }
                            },
                        );
                    }

                    original_index += 1;
                    insert_index += 1;
                }
            } else {
                // Pushing a new value
                diff.log_change(
                    updated.0.estimated_bytes
                        + updated.1.estimated_bytes
                        + estimate_usize_bytes(insert_index),
                    || {
                        let owned = updated.into_owned();
                        Change::InsertMapping {
                            index: insert_index,
                            key: owned.0.into(),
                            value: owned.1.into(),
                        }
                    },
                );
                insert_index += 1;
            }
        }

        if original_index < original_values.len() {
            // Extra values, need to truncate.
            diff.log_change(estimate_usize_bytes(insert_index), || Change::Truncate {
                length: insert_index,
            });
        }
    }

    pub fn apply<T: Serialize + DeserializeOwned>(&self, against: &T) -> Result<T, Error> {
        let updated_value = self.apply_to_value(Value::from_serialize(against))?;
        updated_value.deserialize_as().map_err(Error::from)
    }

    pub fn apply_to_value(&self, mut value: Value<'static>) -> Result<Value<'static>, Error> {
        let mut changes = self.changes.iter().cloned();
        let apply_result = match changes.next() {
            Some(Change::Replace { index: None, value }) => ApplyResult::Replace(value),
            Some(Change::EnterSequence {
                index: None,
                key: false,
            }) => {
                if let Value::Sequence(sequence) = &mut value {
                    apply_changes_to_sequence(sequence, &mut changes)?
                } else {
                    todo!("error")
                }
            }
            Some(Change::EnterMap {
                index: None,
                key: false,
            }) => {
                if let Value::Mappings(mappings) = &mut value {
                    apply_changes_to_mappings(mappings, &mut changes)?
                } else {
                    todo!("error")
                }
            }
            None => ApplyResult::Ok,
            _ => todo!("error"),
        };

        match apply_result {
            ApplyResult::Ok => Ok(value),
            ApplyResult::Replace(new_value) => Ok(new_value),
        }
    }

    // fn serialize_into<W: Write>(&self, writer: W) -> io::Result<()> {

    // }
}

impl Display for Diff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        enum StackEntry {
            Sequence,
            Map,
        }
        let mut stack = Vec::new();
        for change in &self.changes {
            match change {
                Change::EnterSequence { index, key } => {
                    if let Some(index) = index {
                        if *key {
                            write!(f, "[@{index};")?;
                        } else {
                            write!(f, "[{index};")?;
                        }
                    } else {
                        f.write_str("[;")?;
                    }
                    stack.push(StackEntry::Sequence);
                }
                Change::EnterMap { index, key } => {
                    if let Some(index) = index {
                        if *key {
                            write!(f, "{{@{index};")?;
                        } else {
                            write!(f, "{{{index};")?;
                        }
                    } else {
                        f.write_str("{;")?;
                    }
                    stack.push(StackEntry::Map);
                }
                Change::Exit => match stack.pop() {
                    Some(StackEntry::Sequence) => f.write_char(']')?,
                    Some(StackEntry::Map) => f.write_char('}')?,
                    None => f.write_char('?')?,
                },
                Change::Replace {
                    index: Some(index),
                    value,
                } => write!(f, "~{index};{}", ValueDisplay(value))?,
                Change::ReplaceKey { index, key } => write!(f, "~@{index};{}", ValueDisplay(key))?,
                Change::Replace { index: None, value } => write!(f, "~;{}", ValueDisplay(value))?,
                Change::ReplaceMapping { index, key, value } => {
                    write!(f, "~{index};{};{}", ValueDisplay(key), ValueDisplay(value))?
                }
                Change::Remove { index, length } => write!(f, "-{index};{length}")?,
                Change::Truncate { length } => write!(f, "${length}")?,
                Change::Insert { index, value } => write!(f, "+{index};{value}")?,
                Change::InsertMapping { index, key, value } => {
                    write!(f, "+{index};{};{}", ValueDisplay(key), ValueDisplay(value))?
                }
            }
        }
        Ok(())
    }
}

fn apply_changes_to_sequence(
    values: &mut Vec<Value<'static>>,
    changes: &mut Cloned<slice::Iter<'_, Change>>,
) -> Result<ApplyResult, Error> {
    loop {
        match changes.next() {
            Some(Change::Replace {
                index: Some(index),
                value,
            }) => {
                values[index] = value;
            }
            Some(Change::Remove { index, length }) => {
                if index + length <= values.len() {
                    values.drain(index..index + length);
                } else {
                    todo!("error")
                }
            }
            Some(Change::Truncate { length }) => {
                if length <= values.len() {
                    values.truncate(length);
                } else {
                    todo!("error")
                }
            }
            Some(Change::Insert { index, value }) => {
                if index <= values.len() {
                    values.insert(index, value);
                } else {
                    todo!("error")
                }
            }
            Some(Change::EnterSequence {
                index: Some(index),
                key: false,
            }) => {
                if let Some(Value::Sequence(entered)) = values.get_mut(index) {
                    apply_changes_to_sequence(entered, changes)?;
                } else {
                    todo!("error")
                }
            }
            Some(Change::EnterMap {
                index: Some(index),
                key: false,
            }) => {
                if let Some(Value::Mappings(entered)) = values.get_mut(index) {
                    apply_changes_to_mappings(entered, changes)?;
                } else {
                    todo!("error")
                }
            }
            Some(Change::Exit) | None => return Ok(ApplyResult::Ok),
            _ => todo!("error"),
        };
    }
}

fn apply_changes_to_mappings(
    values: &mut Vec<(Value<'static>, Value<'static>)>,
    changes: &mut Cloned<slice::Iter<'_, Change>>,
) -> Result<ApplyResult, Error> {
    loop {
        match changes.next() {
            Some(Change::ReplaceMapping { index, key, value }) => {
                values[index] = (key, value);
            }
            Some(Change::Replace {
                index: Some(index),
                value,
            }) => {
                values[index].1 = value;
            }
            Some(Change::ReplaceKey { index, key }) => {
                values[index].0 = key;
            }
            Some(Change::Remove { index, length }) => {
                if index + length <= values.len() {
                    values.drain(index..index + length);
                } else {
                    todo!("error")
                }
            }
            Some(Change::Truncate { length }) => {
                if length <= values.len() {
                    values.truncate(length);
                } else {
                    todo!("error")
                }
            }
            Some(Change::InsertMapping { index, key, value }) => {
                if index <= values.len() {
                    values.insert(index, (key, value));
                } else {
                    todo!("error")
                }
            }
            Some(Change::EnterSequence {
                index: Some(index),
                key,
            }) => {
                if let Some(Value::Sequence(entered)) =
                    values
                        .get_mut(index)
                        .map(|pair| if key { &mut pair.0 } else { &mut pair.1 })
                {
                    apply_changes_to_sequence(entered, changes)?;
                } else {
                    todo!("error")
                }
            }
            Some(Change::EnterMap {
                index: Some(index),
                key,
            }) => {
                if let Some(Value::Mappings(entered)) =
                    values
                        .get_mut(index)
                        .map(|pair| if key { &mut pair.0 } else { &mut pair.1 })
                {
                    apply_changes_to_mappings(entered, changes)?;
                } else {
                    todo!("error")
                }
            }
            Some(Change::Exit) | None => return Ok(ApplyResult::Ok),
            _ => todo!("error"),
        };
    }
}

enum ApplyResult {
    Ok,
    Replace(Value<'static>),
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("error deserializing Value: {0}")]
    ValueDeserialization(#[from] pot::ValueError),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    EnterSequence {
        index: Option<usize>,
        key: bool,
    },
    EnterMap {
        index: Option<usize>,
        key: bool,
    },
    Exit,
    Replace {
        index: Option<usize>,
        value: Value<'static>,
    },
    ReplaceKey {
        index: usize,
        key: Value<'static>,
    },
    ReplaceMapping {
        index: usize,
        key: Value<'static>,
        value: Value<'static>,
    },
    Remove {
        index: usize,
        length: usize,
    },
    Truncate {
        length: usize,
    },
    Insert {
        index: usize,
        value: Value<'static>,
    },
    InsertMapping {
        index: usize,
        key: Value<'static>,
        value: Value<'static>,
    },
}

trait Differ {
    fn log_change<F: FnOnce() -> Change>(&mut self, estimated_bytes: usize, change: F);
}

#[derive(Default)]
struct Counter {
    estimated_bytes: usize,
}

impl Differ for Counter {
    fn log_change<F: FnOnce() -> Change>(&mut self, estimated_bytes: usize, _change: F) {
        self.estimated_bytes += 1 + estimated_bytes;
    }
}

impl Differ for Diff {
    fn log_change<F: FnOnce() -> Change>(&mut self, _estimated_bytes: usize, change: F) {
        self.changes.push(change());
    }
}

#[derive(Clone)]
struct Estimated {
    estimated_bytes: usize,
    value: EstimatedValue,
}

impl Estimated {
    fn new(commands: usize, data_bytes: usize, value: EstimatedValue) -> Self {
        Self {
            estimated_bytes: commands + data_bytes,
            value,
        }
    }
}

impl<'a> PartialEq<Value<'a>> for Estimated {
    fn eq(&self, other: &Value<'a>) -> bool {
        match (&self.value, other) {
            (EstimatedValue::None, Value::None) | (EstimatedValue::Unit, Value::Unit) => true,
            (EstimatedValue::Bool(a), Value::Bool(b)) => a == b,
            (EstimatedValue::Integer(a), Value::Integer(b)) => a == b,
            (EstimatedValue::Float(a), Value::Float(b)) => a == b,
            (EstimatedValue::Bytes(a), Value::Bytes(b)) => a == b,
            (EstimatedValue::String(a), Value::String(b)) => a == b,
            (EstimatedValue::Sequence(a), Value::Sequence(b)) => a == b,
            (EstimatedValue::Mappings(a), Value::Mappings(b)) => a
                .iter()
                .zip(b.iter())
                .all(|(a, b)| a.0 == b.0 && a.1 == b.1),
            _ => false,
        }
    }
}

#[derive(Clone)]
enum EstimatedValue {
    /// A value representing None.
    None,
    /// A value representing a Unit (`()`).
    Unit,
    /// A boolean value
    Bool(bool),
    /// An integer value.
    Integer(Integer),
    /// A floating point value.
    Float(Float),
    /// A value containing arbitrary bytes.
    Bytes(Cow<'static, [u8]>),
    /// A string value.
    String(Cow<'static, str>),
    /// A sequence of values.
    Sequence(VecDeque<Estimated>),
    /// A sequence of key-value mappings.
    Mappings(VecDeque<(Estimated, Estimated)>),
}

impl From<Value<'static>> for Estimated {
    fn from(value: Value<'static>) -> Self {
        match value {
            Value::None => Self::new(1, 0, EstimatedValue::None),
            Value::Unit => Self::new(1, 0, EstimatedValue::Unit),
            Value::Bool(bool) => Self::new(1, 1, EstimatedValue::Bool(bool)),
            Value::Integer(integer) => {
                Self::new(1, integer_size(integer), EstimatedValue::Integer(integer))
            }
            Value::Float(float) => Self::new(
                1,
                if float.as_f32().is_ok() { 4 } else { 8 },
                EstimatedValue::Float(float),
            ),
            Value::Bytes(bytes) => Self::new(1, bytes.len(), EstimatedValue::Bytes(bytes)),
            Value::String(string) => Self::new(1, string.len(), EstimatedValue::String(string)),
            Value::Sequence(values) => {
                let values: VecDeque<Self> = values.into_iter().map(Self::from).collect();
                Self::new(
                    values.len() + 1,
                    values.iter().map(|v| v.estimated_bytes).sum::<usize>(),
                    EstimatedValue::Sequence(values),
                )
            }
            Value::Mappings(mappings) => {
                let mappings: VecDeque<(Self, Self)> = mappings
                    .into_iter()
                    .map(|(key, value)| (Self::from(key), Self::from(value)))
                    .collect();
                Self::new(
                    mappings.len() * 2 + 1,
                    mappings
                        .iter()
                        .map(|(key, value)| key.estimated_bytes + value.estimated_bytes)
                        .sum::<usize>(),
                    EstimatedValue::Mappings(mappings),
                )
            }
        }
    }
}
impl From<Estimated> for Value<'static> {
    fn from(value: Estimated) -> Self {
        Self::from(value.value)
    }
}

impl From<EstimatedValue> for Value<'static> {
    fn from(value: EstimatedValue) -> Self {
        match value {
            EstimatedValue::None => Value::None,
            EstimatedValue::Unit => Value::Unit,
            EstimatedValue::Bool(bool) => Value::Bool(bool),
            EstimatedValue::Integer(integer) => Value::Integer(integer),
            EstimatedValue::Float(float) => Value::Float(float),
            EstimatedValue::Bytes(bytes) => Value::Bytes(bytes),
            EstimatedValue::String(string) => Value::String(string),
            EstimatedValue::Sequence(sequence) => {
                Value::Sequence(sequence.into_iter().map(Self::from).collect())
            }
            EstimatedValue::Mappings(mappings) => Value::Mappings(
                mappings
                    .into_iter()
                    .map(|(key, value)| (Self::from(key), Self::from(value)))
                    .collect(),
            ),
        }
    }
}

fn integer_size(integer: Integer) -> usize {
    if integer.as_i8().is_ok() || integer.as_u8().is_ok() {
        1
    } else if integer.as_i16().is_ok() || integer.as_u16().is_ok() {
        2
    } else if integer.as_i32().is_ok() || integer.as_u32().is_ok() {
        4
    } else if integer.as_i64().is_ok() || integer.as_u64().is_ok() {
        8
    } else {
        16
    }
}

enum CowDeque<'a, T> {
    Owned(VecDeque<T>),
    Borrowed {
        deque: &'a VecDeque<T>,
        index: usize,
    },
}

impl<'a, T> CowDeque<'a, T>
where
    T: Clone,
{
    fn pop_front(&mut self) -> Option<Cow<'a, T>> {
        match self {
            CowDeque::Owned(queue) => queue.pop_front().map(Cow::Owned),
            CowDeque::Borrowed { deque, index } => {
                if let Some(borrowed) = deque.get(*index) {
                    *index += 1;
                    Some(Cow::Borrowed(borrowed))
                } else {
                    None
                }
            }
        }
    }

    fn iter(&self) -> iter::Skip<vec_deque::Iter<'_, T>> {
        match self {
            CowDeque::Owned(queue) => queue.iter().skip(0),
            CowDeque::Borrowed { deque, index } => deque.iter().skip(*index),
        }
    }
}

#[derive(Debug)]
pub struct Diffable<T> {
    active: T,
    dirty: bool,
    latest: Value<'static>,
}

impl<T> Diffable<T>
where
    T: Serialize + DeserializeOwned,
{
    pub fn new(value: T) -> Self {
        let latest = Value::from_serialize(&value);
        Self {
            latest,
            active: value,
            dirty: false,
        }
    }

    pub fn diff(&mut self) -> Option<Diff> {
        if self.dirty {
            self.dirty = false;
            // TODO make a Value method to recycle buffers yet reload from a Serialize.
            let updated = Value::from_serialize(&self.active);
            // TODO this shouldn't be a clone.
            let diff = Diff::between_values(&self.latest, updated.clone());
            self.latest = updated;
            if diff.changes.is_empty() {
                None
            } else {
                Some(diff)
            }
        } else {
            None
        }
    }
}

impl<T> Deref for Diffable<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.active
    }
}

impl<T> DerefMut for Diffable<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.dirty = true;
        &mut self.active
    }
}

#[cfg(test)]
mod tests;

const fn estimate_usize_bytes(value: usize) -> usize {
    const U8_MAX_AS_USIZE: usize = u8::MAX as usize;
    const U8_MAX_PLUS_1_AS_USIZE: usize = U8_MAX_AS_USIZE + 1;
    const U16_MAX_AS_USIZE: usize = u16::MAX as usize;
    const U16_MAX_PLUS_1_AS_USIZE: usize = U16_MAX_AS_USIZE + 1;
    const U32_MAX_AS_USIZE: usize = u32::MAX as usize;
    match value {
        0..=U8_MAX_AS_USIZE => 1,
        U8_MAX_PLUS_1_AS_USIZE..=U16_MAX_AS_USIZE => 2,
        #[cfg(target_pointer_width = "32")]
        _ => 4,

        #[cfg(target_pointer_width = "64")]
        U16_MAX_PLUS_1_AS_USIZE..=U32_MAX_AS_USIZE => 4,
        #[cfg(target_pointer_width = "64")]
        _ => 8,
    }
}
