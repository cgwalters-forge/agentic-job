//! The framing of an `outputs.jsonl`, read-only: its requests in order,
//! each its `type` and its JSON object as parsed, unchanged and in key
//! order. Escapes are decoded, numbers read as serde_json reads them, and
//! a key given twice keeps its last value, as in JavaScript.
//!
//! [`decode`] is for a caller that admits requests by rules of its own and
//! needs only the framing: the whole input at most [`MAX_OUTPUTS_BYTES`],
//! UTF-8, blank lines skipped and each other line a JSON object with a
//! string `type`. It sanitizes nothing and knows no type, limit or forge;
//! [`collect`](crate::collect) frames lines with the same functions and
//! then validates and sanitizes each request as gh-aw's collector does.
//!
//! Lines are split and numbered as gh-aw's collector numbers them: the
//! input with JavaScript's whitespace trimmed from both ends, split at
//! `\n`, each line trimmed the same way (so CRLF line ends are taken). A
//! leading blank line is therefore not counted.
//!
//! The input may carry credentials, so an error names the line and the
//! kind of failure, and quotes nothing of it.

use std::fmt;
use std::str::Utf8Error;

use serde_json::{Map, Value};

use crate::collect::sanitize::js_trim;
use crate::vocabulary::MAX_OUTPUTS_BYTES;

/// One request as written: its `type`, and the whole object, that `type`
/// among its keys.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub kind: String,
    pub object: Map<String, Value>,
}

/// Why [`decode`] refused its input. LINE numbers as the collector does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The input is over [`MAX_OUTPUTS_BYTES`].
    TooLarge,
    /// The input is not UTF-8; the error gives only offsets.
    NotUtf8(Utf8Error),
    InvalidJson {
        line: usize,
    },
    NotObject {
        line: usize,
    },
    MissingType {
        line: usize,
    },
    TypeNotString {
        line: usize,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "safe outputs are over {MAX_OUTPUTS_BYTES} bytes"),
            Self::NotUtf8(_) => write!(f, "safe outputs are not UTF-8"),
            Self::InvalidJson { line } => write!(f, "safe outputs line {line}: not JSON"),
            Self::NotObject { line } => write!(f, "safe outputs line {line}: not a JSON object"),
            Self::MissingType { line } => write!(f, "safe outputs line {line}: no 'type' field"),
            Self::TypeNotString { line } => {
                write!(f, "safe outputs line {line}: 'type' is not a string")
            }
        }
    }
}

impl std::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotUtf8(err) => Some(err),
            _ => None,
        }
    }
}

/// The requests of BYTES, an `outputs.jsonl` as read, in order, or why it
/// is refused whole: no request is returned from input with any line
/// malformed.
pub fn decode(bytes: &[u8]) -> Result<Vec<Request>, DecodeError> {
    lines(text(bytes)?)
        .map(|(line, text)| {
            frame(text).map_err(|malformed| match malformed {
                Malformed::Json(_) => DecodeError::InvalidJson { line },
                Malformed::NotObject => DecodeError::NotObject { line },
                Malformed::MissingType => DecodeError::MissingType { line },
                Malformed::TypeNotString(_) => DecodeError::TypeNotString { line },
            })
        })
        .collect()
}

/// BYTES as text, if within [`MAX_OUTPUTS_BYTES`] and UTF-8; the size is
/// checked before anything is read of them.
pub(crate) fn text(bytes: &[u8]) -> Result<&str, DecodeError> {
    if !u64::try_from(bytes.len()).is_ok_and(|len| len <= MAX_OUTPUTS_BYTES) {
        return Err(DecodeError::TooLarge);
    }
    std::str::from_utf8(bytes).map_err(DecodeError::NotUtf8)
}

/// The lines of TEXT that are not blank, trimmed, with their numbers.
pub(crate) fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    js_trim(text)
        .split('\n')
        .enumerate()
        .map(|(index, line)| (index + 1, js_trim(line)))
        .filter(|(_, line)| !line.is_empty())
}

/// Why a line is not a request, with what the collector words its
/// refusal from: the parser's error, or the `type` that is not a string.
pub(crate) enum Malformed {
    Json(serde_json::Error),
    NotObject,
    MissingType,
    TypeNotString(Value),
}

/// The request on one line, TEXT.
pub(crate) fn frame(text: &str) -> Result<Request, Malformed> {
    let object = match serde_json::from_str(text) {
        Ok(Value::Object(object)) => object,
        Ok(_) => return Err(Malformed::NotObject),
        Err(err) => return Err(Malformed::Json(err)),
    };
    let kind = match object.get("type") {
        Some(Value::String(kind)) => kind.clone(),
        Some(kind) => return Err(Malformed::TypeNotString(kind.clone())),
        None => return Err(Malformed::MissingType),
    };
    Ok(Request { kind, object })
}

#[cfg(test)]
mod tests {
    use super::DecodeError::*;
    use super::*;
    use crate::vocabulary::Forge;

    #[test]
    #[expect(invalid_from_utf8, reason = "the error decode returns is expected")]
    fn decode_cases() {
        let body = "<b>@someone</b> fixes #1 https://evil.example/x";
        let sanitized = format!(r#"{{"type":"add_comment","body":"{body}","z":1,"a":2}}"#);
        let config = serde_json::from_str(r#"{"add_comment":{}}"#).unwrap();
        let collected = crate::collect::collect(&sanitized, &config, Forge::Github, &|_| false);
        assert_ne!(collected.items[0]["body"], body, "{collected:?}");
        let mixed = concat!(
            r#"{"type":"create_issue","title":"t","body":"b"}"#,
            "\n",
            r#"{"type":"noop","message":"m"}"#,
            "\n",
            r#"{"type":"anything-at-all","x":[1,{"y":null}]}"#,
        );
        let not_utf8: &[u8] = b"{\"type\":\"a\",\"b\":\"\xff\"}";
        let utf8_error = std::str::from_utf8(not_utf8).unwrap_err();
        type Case<'a> = (&'a str, &'a [u8], Result<&'a [&'a str], DecodeError>);
        let cases: &[Case<'_>] = &[
            ("empty", b"", Ok(&[])),
            ("only blank", b"\n \r\n\t\n", Ok(&[])),
            (
                "mixed types in order",
                mixed.as_bytes(),
                Ok(&["create_issue", "noop", "anything-at-all"]),
            ),
            (
                "blank lines and CRLF",
                b"\r\n{\"type\":\"a\"}\r\n\r\n  \r\n{\"type\":\"b\"}\r\n",
                Ok(&["a", "b"]),
            ),
            (
                "unsanitized body, key order kept",
                sanitized.as_bytes(),
                Ok(&["add_comment"]),
            ),
            ("empty type is a string", br#"{"type":""}"#, Ok(&[""])),
            ("invalid UTF-8", not_utf8, Err(NotUtf8(utf8_error))),
            (
                "invalid JSON, line counted past blank lines",
                b"{\"type\":\"a\"}\n\n{\"type\":",
                Err(InvalidJson { line: 3 }),
            ),
            ("array", b"[1]", Err(NotObject { line: 1 })),
            ("string", b"\"x\"", Err(NotObject { line: 1 })),
            (
                "missing type",
                b"{\"type\":\"a\"}\n{\"body\":\"x\"}",
                Err(MissingType { line: 2 }),
            ),
            ("null", br#"{"type":null}"#, Err(TypeNotString { line: 1 })),
            ("number", br#"{"type":1}"#, Err(TypeNotString { line: 1 })),
            ("list", br#"{"type":[]}"#, Err(TypeNotString { line: 1 })),
        ];
        for (name, input, expected) in cases {
            match (decode(input), expected) {
                (Ok(requests), Ok(kinds)) => {
                    let got: Vec<_> = requests.iter().map(|r| r.kind.as_str()).collect();
                    assert_eq!(got, kinds.to_vec(), "{name}");
                    // Key order and every byte of each field are as written.
                    let written: Vec<_> = requests
                        .iter()
                        .map(|r| serde_json::to_string(&r.object).unwrap())
                        .collect();
                    let text = std::str::from_utf8(input).unwrap();
                    let as_written: Vec<_> = lines(text).map(|(_, line)| line).collect();
                    assert_eq!(written, as_written, "{name}");
                }
                (got, expected) => assert_eq!(got.err(), expected.clone().err(), "{name}"),
            }
        }
    }

    #[test]
    fn decode_bound() {
        let max = usize::try_from(MAX_OUTPUTS_BYTES).unwrap();
        let line = b"{\"type\":\"noop\"}\n";
        let mut input = line.repeat(max / line.len());
        input.resize(max, b' ');
        assert_eq!(decode(&input).map(|r| r.len()), Ok(max / line.len()));
        input.push(b' ');
        assert_eq!(decode(&input), Err(DecodeError::TooLarge));
        // Refused before it is read: invalid UTF-8 past the bound too.
        input.push(0xff);
        assert_eq!(decode(&input), Err(DecodeError::TooLarge));
    }

    #[test]
    fn errors_quote_nothing() {
        let secret = "hunter2-credential";
        let cases = [
            format!("{{\"type\":\"a\",\"t\":\"{secret}"),
            secret.to_owned(),
            format!("[\"{secret}\"]"),
            format!("{{\"body\":\"{secret}\"}}"),
            format!("{{\"type\":[\"{secret}\"]}}"),
        ];
        let not_utf8 = [format!("\"{secret}").into_bytes(), vec![0xff]].concat();
        let inputs = cases.iter().map(|case| case.as_bytes().to_vec());
        for input in inputs.chain([not_utf8]) {
            let err = decode(&input).unwrap_err();
            let shown = format!("{err} {err:?}");
            assert!(!shown.contains(secret), "{shown}");
        }
    }
}
