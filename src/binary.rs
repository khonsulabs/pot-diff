//! This binary format uses 1 byte per change, and uses Pot with
//! `without_header` to encode values as needed. Because this disables Pot's
//! version checking, we include a single byte at the start for future
//! versioning needs.
//!
//! After the version byte is an variable integer describing how many changes
//! are in the diff. After that each change is serialized with no padding.
//! Finally, 4 additional bytes are a CRC32 of the diff to add some security in
//! parsing a slightly incorrect diff.
//!
//! The Change byte uses the top for bits for the variant id. The lower 4 bits
//! are able to encode additional change-specific information.
use std::borrow::Cow;
use std::io::{self, Read, Write};

use ordered_varint::Variable;
use pot::format::Nucleus;
use pot::reader::SliceReader;
use pot::Value;

use crate::{Change, Diff};

const VERSION: u8 = 0;
// const HEADER_FLAG_CRC: u8 = 1 << 7;

const KEY_FLAG: u8 = 1 << 0;
const ROOT_FLAG: u8 = 1 << 1;
const MAPPING_FLAG: u8 = 1 << 2;

const ENTER_SEQUENCE: u8 = 0;
const ENTER_MAP: u8 = 1;
const EXIT: u8 = 2;
const REPLACE: u8 = 3;
const REMOVE: u8 = 4;
const TRUNCATE: u8 = 5;
const INSERT: u8 = 6;

pub fn encode<W: Write>(diff: &Diff, mut writer: W) -> io::Result<()> {
    writer.write_all(&[VERSION])?;
    diff.changes.len().encode_variable(&mut writer)?;
    for change in &diff.changes {
        match change {
            Change::EnterSequence { index, key } => {
                let mut flags = 0;
                if *key {
                    flags |= KEY_FLAG;
                }
                if index.is_none() {
                    flags |= ROOT_FLAG
                }
                write_change_byte(&mut writer, ENTER_SEQUENCE, flags)?;
                if let Some(index) = index {
                    index.encode_variable(&mut writer)?;
                }
            }
            Change::EnterMap { index, key } => {
                let mut flags = 0;
                if *key {
                    flags |= KEY_FLAG;
                }
                if index.is_none() {
                    flags |= ROOT_FLAG
                }
                write_change_byte(&mut writer, ENTER_MAP, flags)?;
                if let Some(index) = index {
                    index.encode_variable(&mut writer)?;
                }
            }
            Change::Exit => {
                write_change_byte(&mut writer, EXIT, 0)?;
            }
            Change::Replace { index, value } => {
                let mut flags = 0;
                if index.is_none() {
                    flags |= ROOT_FLAG
                }
                write_change_byte(&mut writer, REPLACE, flags)?;
                if let Some(index) = index {
                    index.encode_variable(&mut writer)?;
                }
                write_value(&mut writer, value)?;
            }
            Change::ReplaceKey { index, key } => {
                write_change_byte(&mut writer, REPLACE, KEY_FLAG)?;
                index.encode_variable(&mut writer)?;
                write_value(&mut writer, key)?;
            }
            Change::ReplaceMapping { index, key, value } => {
                write_change_byte(&mut writer, REPLACE, MAPPING_FLAG)?;
                index.encode_variable(&mut writer)?;
                write_value(&mut writer, key)?;
                write_value(&mut writer, value)?;
            }
            Change::Remove { index, length } => {
                write_change_byte(&mut writer, REMOVE, 0)?;
                index.encode_variable(&mut writer)?;
                length.encode_variable(&mut writer)?;
            }
            Change::Truncate { length } => {
                write_change_byte(&mut writer, TRUNCATE, 0)?;
                length.encode_variable(&mut writer)?;
            }
            Change::Insert { index, value } => {
                write_change_byte(&mut writer, INSERT, 0)?;
                index.encode_variable(&mut writer)?;
                write_value(&mut writer, value)?;
            }
            Change::InsertMapping { index, key, value } => {
                write_change_byte(&mut writer, INSERT, MAPPING_FLAG)?;
                index.encode_variable(&mut writer)?;
                write_value(&mut writer, key)?;
                write_value(&mut writer, value)?;
            }
        }
    }
    Ok(())
}

fn write_change_byte<W: Write>(mut writer: W, variant: u8, extra_info: u8) -> io::Result<()> {
    debug_assert!(variant < 16);
    debug_assert!(extra_info < 16);
    writer.write_all(&[(variant << 4) | extra_info])
}

fn write_value<W: Write>(writer: &mut W, value: &Value<'_>) -> io::Result<()> {
    match value {
        Value::None => {
            pot::format::write_none(writer)?;
        }
        Value::Unit => {
            pot::format::write_unit(writer)?;
        }
        Value::Bool(value) => {
            pot::format::write_bool(writer, *value)?;
        }
        Value::Integer(integer) => {
            integer.write_to(writer)?;
        }
        Value::Float(float) => {
            float.write_to(writer)?;
        }
        Value::Bytes(bytes) => {
            pot::format::write_bytes(writer, bytes)?;
        }
        Value::String(str) => {
            pot::format::write_str(writer, str)?;
        }
        Value::Sequence(sequence) => {
            // TODO as cast
            pot::format::write_atom_header(
                &mut *writer,
                pot::format::Kind::Sequence,
                Some(sequence.len() as u64),
            )?;
            for value in sequence {
                write_value(writer, value)?;
            }
        }
        Value::Mappings(mappings) => {
            pot::format::write_atom_header(
                &mut *writer,
                pot::format::Kind::Map,
                Some(mappings.len() as u64),
            )?;
            for (key, value) in mappings {
                write_value(writer, key)?;
                write_value(writer, value)?;
            }
        }
    }

    Ok(())
}

pub fn decode(bytes: &[u8]) -> Result<Diff, DecodeError> {
    let mut bytes = SliceReader::from(bytes);
    let header = read_byte(&mut bytes)?;
    if header & 0x7F != 0 {
        Err(DecodeError::UnsupportedVersion)
    } else {
        let number_of_changes = usize::decode_variable(&mut bytes)?;
        // Basic sanity check: the diff can't have more changes than bytes.
        if number_of_changes > bytes.len() {
            return Err(DecodeError::InvalidData);
        }

        let mut diff = Diff {
            changes: Vec::with_capacity(number_of_changes),
        };
        for _ in 0..number_of_changes {
            diff.changes.push(read_change(&mut bytes)?);
        }
        Ok(diff)
    }
}

fn check_bit(source: u8, flag: u8) -> bool {
    (source & flag) != 0
}

fn read_change(bytes: &mut SliceReader<'_>) -> Result<Change, DecodeError> {
    let header = read_byte(bytes)?;
    let variant = header >> 4;
    match variant {
        ENTER_SEQUENCE => {
            let key = check_bit(header, KEY_FLAG);
            let is_root = check_bit(header, ROOT_FLAG);
            let index = if is_root {
                None
            } else {
                Some(usize::decode_variable(&mut *bytes)?)
            };
            Ok(Change::EnterSequence { index, key })
        }
        ENTER_MAP => {
            let key = check_bit(header, KEY_FLAG);
            let is_root = check_bit(header, ROOT_FLAG);
            let index = if is_root {
                None
            } else {
                Some(usize::decode_variable(&mut *bytes)?)
            };
            Ok(Change::EnterMap { index, key })
        }
        EXIT => Ok(Change::Exit),
        REPLACE => {
            let key = check_bit(header, KEY_FLAG);
            let is_root = check_bit(header, ROOT_FLAG);
            let is_mapping = check_bit(header, MAPPING_FLAG);
            match (is_root, key, is_mapping) {
                (_, false, false) => {
                    let index = if is_root {
                        None
                    } else {
                        Some(usize::decode_variable(&mut *bytes)?)
                    };
                    let value = read_value(bytes)?;
                    Ok(Change::Replace { index, value })
                }
                (false, true, false) => {
                    let index = usize::decode_variable(&mut *bytes)?;
                    let key = read_value(bytes)?;
                    Ok(Change::ReplaceKey { index, key })
                }
                (false, false, true) => {
                    let index = usize::decode_variable(&mut *bytes)?;
                    let key = read_value(bytes)?;
                    let value = read_value(bytes)?;
                    Ok(Change::ReplaceMapping { index, key, value })
                }
                _ => Err(DecodeError::InvalidData),
            }
        }
        REMOVE => {
            let index = usize::decode_variable(&mut *bytes)?;
            let length = usize::decode_variable(&mut *bytes)?;
            Ok(Change::Remove { index, length })
        }
        TRUNCATE => {
            let length = usize::decode_variable(&mut *bytes)?;
            Ok(Change::Truncate { length })
        }
        INSERT => {
            let is_mapping = check_bit(header, MAPPING_FLAG);
            let index = usize::decode_variable(&mut *bytes)?;
            let key = read_value(bytes)?;
            if is_mapping {
                let value = read_value(bytes)?;

                Ok(Change::InsertMapping { index, key, value })
            } else {
                Ok(Change::Insert { index, value: key })
            }
        }
        _ => Err(DecodeError::InvalidData),
    }
}

fn read_value(bytes: &mut SliceReader<'_>) -> Result<Value<'static>, DecodeError> {
    #[allow(const_item_mutation)] // it is intentional
    let atom = pot::format::read_atom(bytes, &mut usize::MAX)?;
    match atom.kind {
        pot::format::Kind::Special => match atom.nucleus {
            Some(Nucleus::Unit) => Ok(Value::Unit),
            Some(Nucleus::Boolean(bool)) => Ok(Value::Bool(bool)),
            None => Ok(Value::None),
            _ => Err(DecodeError::InvalidData),
        },
        pot::format::Kind::Int | pot::format::Kind::UInt => {
            if let Some(Nucleus::Integer(integer)) = atom.nucleus {
                Ok(Value::Integer(integer))
            } else {
                Err(DecodeError::InvalidData)
            }
        }
        pot::format::Kind::Float => {
            if let Some(Nucleus::Float(float)) = atom.nucleus {
                Ok(Value::Float(float))
            } else {
                Err(DecodeError::InvalidData)
            }
        }
        pot::format::Kind::Sequence => {
            let length = atom.arg as usize;
            if length < bytes.len() {
                let mut values = Vec::with_capacity(length);
                for _ in 0..length {
                    values.push(read_value(bytes)?);
                }
                Ok(Value::Sequence(values))
            } else {
                Err(DecodeError::InvalidData)
            }
        }
        pot::format::Kind::Map => {
            let length = atom.arg as usize;
            if length < bytes.len() {
                let mut values = Vec::with_capacity(length);
                for _ in 0..length {
                    let key = read_value(bytes)?;
                    let value = read_value(bytes)?;
                    values.push((key, value));
                }
                Ok(Value::Mappings(values))
            } else {
                Err(DecodeError::InvalidData)
            }
        }
        pot::format::Kind::Symbol => Err(DecodeError::InvalidData),
        pot::format::Kind::Bytes => {
            if let Some(Nucleus::Bytes(bytes)) = atom.nucleus {
                if let Ok(str) = std::str::from_utf8(&bytes) {
                    Ok(Value::String(Cow::Owned(str.to_string())))
                } else {
                    Ok(Value::Bytes(Cow::Owned(bytes.to_vec())))
                }
            } else {
                Err(DecodeError::InvalidData)
            }
        }
    }
}

fn read_byte(bytes: &mut SliceReader<'_>) -> Result<u8, DecodeError> {
    let mut byte = [0];
    bytes.read_exact(&mut byte)?;
    Ok(byte[0])
}

#[derive(thiserror::Error, Debug)]
pub enum DecodeError {
    #[error("unsupported diff version")]
    UnsupportedVersion,
    #[error("the diff ended unexpectedly")]
    UnexpectedEof,
    #[error("the diff contained invalid data")]
    InvalidData,
    #[error("a value failed to deserialize: {0}")]
    Pot(#[from] pot::Error),
}

impl From<io::Error> for DecodeError {
    fn from(value: io::Error) -> Self {
        match value.kind() {
            io::ErrorKind::UnexpectedEof => Self::UnexpectedEof,
            _ => Self::InvalidData,
        }
    }
}
