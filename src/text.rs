use std::fmt::{self, Display, Write};
use std::io;

use pot::Value;

pub struct ValueDisplay<'a>(pub &'a Value<'a>);

impl<'a> Display for ValueDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Value::None => f.write_str("none"),
            Value::Unit => f.write_str("()"),
            Value::Bool(true) => f.write_str("true"),
            Value::Bool(false) => f.write_str("false"),
            Value::Integer(integer) => integer.fmt(f),
            Value::Float(float) => float.fmt(f),
            Value::Bytes(bytes) => BytesDisplay(bytes).fmt(f),
            Value::String(str) => StringDisplay(str).fmt(f),
            Value::Sequence(sequence) => {
                f.write_char('[')?;

                for (index, value) in sequence.iter().enumerate() {
                    if index > 0 {
                        f.write_char(',')?;
                    }
                    ValueDisplay(value).fmt(f)?;
                }

                f.write_char(']')
            }
            Value::Mappings(mappings) => {
                f.write_char('{')?;

                for (index, (key, value)) in mappings.iter().enumerate() {
                    if index > 0 {
                        f.write_char(',')?;
                    }
                    ValueDisplay(key).fmt(f)?;
                    f.write_char(':')?;
                    ValueDisplay(value).fmt(f)?;
                }

                f.write_char('}')
            }
        }
    }
}

struct StringDisplay<'a>(pub &'a str);

impl<'a> Display for StringDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_char('"')?;
        for ch in self.0.chars() {
            match ch {
                '"' => f.write_str("\\\"")?,
                '\\' => f.write_str("\\\\")?,
                '\n' => f.write_str("\\n")?,
                '\r' => f.write_str("\\r")?,
                '\t' => f.write_str("\\t")?,
                '\0' => f.write_str("\\0")?,
                ' '..=char::MAX => f.write_char(ch)?,
                '\0'..='\u{1f}' => {
                    // hex encode
                    let ch = ch as u8;
                    f.write_str("\\x")?;
                    f.write_char(encode_hex_nibble(ch >> 4) as char)?;
                    f.write_char(encode_hex_nibble(ch & 0xf) as char)?;
                }
            }
        }
        f.write_char('"')
    }
}

pub fn decode_string(string: &str, out: &mut String) -> Result<usize, DecodeError> {
    let mut bytes_read = 0;
    let string = if let Some(string) = string.strip_prefix('"') {
        bytes_read += 1;
        string
    } else {
        string
    };
    let mut chars = string.chars();

    while let Some(char) = chars.next() {
        bytes_read += char.len_utf8();
        match char {
            '\\' => {
                bytes_read += 1;
                match chars.next().ok_or(DecodeError::InvalidEscape)? {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    '0' => out.push('\0'),
                    'x' => {
                        bytes_read += 2;
                        let high_nibble = decode_hex_nibble(
                            chars.next().ok_or(DecodeError::InvalidHexadecimal)? as u8,
                        )?;

                        let low_nibble = decode_hex_nibble(
                            chars.next().ok_or(DecodeError::InvalidHexadecimal)? as u8,
                        )?;
                        let decoded = high_nibble << 4 | low_nibble;
                        if decoded >= b' ' {
                            return Err(DecodeError::InvalidEscape);
                        } else {
                            out.push(decoded as char);
                        }
                    }
                    _ => return Err(DecodeError::InvalidEscape),
                }
            }
            '"' => return Ok(bytes_read),
            _ => {
                out.push(char);
            }
        }
    }

    Err(DecodeError::MissingQuote)
}

#[test]
fn string_display_test() {
    fn test_string_encode(string: &str, expected: &str) {
        assert_eq!(StringDisplay(string).to_string(), expected);

        let mut decoded = String::new();
        decode_string(expected, &mut decoded).unwrap();
        assert_eq!(decoded, string);
    }

    test_string_encode("", r#""""#);
    test_string_encode(" ", r#"" ""#);
    test_string_encode("\r\n\t\0\"\u{7}", r#""\r\n\t\0\"\x07""#);
}

/// Displays bytes using a combination of hexadecimal and printable ascii.
///
/// When a non-safe character is encountered, the mode is switched to
/// hexadecimal. When printable characters are encountered, the mode can be
/// switched back to ascii. Mode switches use the '|' character, which makes it
/// an unprintable character.
struct BytesDisplay<'a>(pub &'a [u8]);

impl<'a> Display for BytesDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn is_printable(ch: u8) -> bool {
            (32..127).contains(&ch) && ch != b'|' && ch != b';'
        }
        f.write_char('#')?;
        let mut in_hex = true;

        let mut bytes = self.0.iter().copied().peekable();
        while let Some(byte) = bytes.next() {
            let current_is_printable = is_printable(byte);
            if in_hex {
                if current_is_printable && bytes.peek().copied().map_or(false, is_printable) {
                    // Switch modes
                    in_hex = false;
                    f.write_char('|')?;
                }
            } else if !current_is_printable {
                in_hex = true;
                f.write_char('|')?;
            }

            if in_hex {
                write!(f, "{byte:02x}")?;
            } else {
                f.write_char(byte as char)?;
            }
        }

        Ok(())
    }
}

pub fn decode_bytes<W: io::Write>(
    encoded: &[u8],
    end_on: Option<u8>,
    mut writer: W,
) -> Result<usize, DecodeError> {
    let mut bytes_read = 0;
    let mut encoded = if encoded.first() == Some(&b'#') {
        bytes_read += 1;
        &encoded[1..]
    } else {
        encoded
    }
    .iter()
    .copied();

    let mut in_hex = true;
    while let Some(byte) = encoded.next() {
        bytes_read += 1;
        if byte == b'|' {
            in_hex = !in_hex;
        } else if end_on == Some(byte) {
            break;
        } else if in_hex {
            // Read hex in pairs
            let second_nibble = encoded.next().ok_or(DecodeError::InvalidHexadecimal)?;
            bytes_read += 1;
            let byte = decode_hex_nibble(byte)? << 4 | decode_hex_nibble(second_nibble)?;
            writer.write_all(&[byte])?;
        } else {
            // Raw bytes
            writer.write_all(&[byte])?;
        }
    }

    Ok(bytes_read)
}

fn encode_hex_nibble(nibble: u8) -> u8 {
    if nibble < 10 {
        b'0' + nibble
    } else {
        b'a' + nibble
    }
}

fn decode_hex_nibble(byte: u8) -> Result<u8, DecodeError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(DecodeError::InvalidHexadecimal),
    }
}

#[test]
fn byte_display_test() {
    fn test_byte_encode(bytes: &[u8], expected: &str) {
        assert_eq!(BytesDisplay(bytes).to_string(), expected);

        let mut decoded = Vec::new();
        decode_bytes(expected.as_bytes(), None, &mut decoded).unwrap();
        assert_eq!(decoded, bytes);
    }

    test_byte_encode(&[], "#");
    test_byte_encode(&[0], "#00");
    test_byte_encode(&[b' '], "#20");
    test_byte_encode(&[b' ', b' '], "#|  ");
    test_byte_encode(&[b' ', b' ', b'|'], "#|  |7c");
    test_byte_encode(&[0xff, 0xff], "#ffff");
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid hexadecimal")]
    InvalidHexadecimal,
    #[error("string missing quote")]
    MissingQuote,
    #[error("invalid escape sequence")]
    InvalidEscape,
    // InvalidInteger(#[from] ParseIntError),
}

// pub fn parse(diff: &str) -> Result<Diff, DecodeError> {
//     let mut chars = diff.chars().peekable();
//     let mut diff = Diff {
//         changes: Vec::new(),
//     };

//     while let Some(ch) = chars.next() {
//         match ch {
//             '[' => {}
//             '{' => {}
//             '~' => {}
//             _ => todo!("error"),
//         }
//     }

//     Ok(diff)
// }

// fn read_usize(
//     chars: &mut Peekable<Chars<'_>>,
//     scratch: &mut String,
// ) -> Result<Option<usize>, DecodeError> {
//     scratch.clear();
//     while let Some(ch) = chars.peek() {
//         if ('0'..='9').contains(&ch) {
//             scratch.push(ch);
//         }
//     }

//     if scratch.is_empty() {
//         Ok(None)
//     } else {
//         scratch.parse().map_err(DecodeError::from)
//     }
// }
