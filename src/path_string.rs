//! Names and paths held as strings without losing a byte.
//!
//! A Unix name is any bytes but `/` and NUL, so it need not be UTF-8 — while a
//! Rust `String` and a protobuf `string` both must be. [`PathString`] is the
//! bridge: an escaped form that is always valid UTF-8 and always decodes back to
//! the exact bytes the filesystem gave.
//!
//! The escape has to escape its own marker, and that is forced rather than
//! chosen. Escaping only the invalid bytes is not invertible: a file really can be
//! named `caf%80.txt` in plain ASCII, and it would escape to itself and collide
//! with the file named `caf` followed by the byte `0x80`. Nor is there a marker
//! character to reserve instead — every valid `String` is a possible filename, so
//! an injective encoding must move some already-valid names off themselves.
//!
//! The tempting alternative is unavailable. Python's `surrogateescape` and
//! Windows' `OsString` map an invalid byte `b` to `U+DC00 + b`, which needs no
//! marker because a lone surrogate cannot occur in valid UTF-8 — but Rust's `char`
//! excludes `U+D800`–`U+DFFF`, so a `String` cannot hold one.
//!
//! | The name has | The escaped form holds |
//! | --- | --- |
//! | a valid UTF-8 run | it, verbatim |
//! | the literal `%` | `%%` |
//! | a byte outside UTF-8 | `%XX`, uppercase hex |
//!
//! Two properties earn their keep elsewhere:
//!
//! - **The escaped form never contains `/`,** and `/` cannot occur inside a name,
//!   so `escape(join(a, b)) == join(escape(a), escape(b))`. Joining and prefix
//!   tests work on escaped paths unchanged.
//! - **The escaped form is canonical,** because decoding accepts only uppercase
//!   hex. Paths are compared and used as map keys as escaped strings, so two
//!   spellings of one file would be two different keys.

use std::{
    borrow::Cow,
    ffi::{OsStr, OsString},
    os::unix::ffi::{OsStrExt as _, OsStringExt as _},
    path::{Component, Path, PathBuf},
};

use thiserror::Error;

/// Marks an escape sequence, and is itself escaped by doubling.
const MARKER: char = '%';

const HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

/// The lowest byte an escape may carry. Every byte below this is valid UTF-8 on
/// its own, so [`escape`] never emits one — and refusing them on the way back is
/// what stops a decoded `%2F` from introducing a separator, or `%00` a NUL, inside
/// what is meant to be a single name.
const LOWEST_ESCAPABLE_BYTE: u8 = 0x80;

/// Why an escaped string could not be decoded.
///
/// Only reachable from text that did not come out of [`escape`], which in
/// practice means a peer that is not speaking this protocol.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum EscapeError {
    #[error("escape sequence is truncated")]
    Truncated,
    #[error("escape sequence has a digit that is not uppercase hexadecimal")]
    NotUppercaseHex,
    #[error(
        "`%{0:02X}` is not an escapable byte: only bytes from 0x{LOWEST_ESCAPABLE_BYTE:02X} are escaped"
    )]
    NotEscapable(u8),
}

/// Escapes bytes into valid UTF-8.
#[must_use]
fn escape(bytes: &[u8]) -> String {
    let mut escaped = String::with_capacity(bytes.len());
    let mut rest = bytes;
    loop {
        let error = match str::from_utf8(rest) {
            Ok(valid) => {
                push_valid(&mut escaped, valid);
                return escaped;
            }
            Err(error) => error,
        };
        let (valid, invalid) = rest.split_at(error.valid_up_to());
        // `valid_up_to` promises this half decodes; the default is unreachable.
        push_valid(&mut escaped, str::from_utf8(valid).unwrap_or_default());
        // `None` means the input stops part-way through a sequence, so everything
        // left is a valid prefix of one and none of it decodes alone.
        let invalid_len = error.error_len().unwrap_or(invalid.len());
        for &byte in &invalid[..invalid_len] {
            // The first byte that fails to decode is never ASCII — an ASCII byte
            // always decodes — and neither a lead byte nor a continuation byte is.
            debug_assert!(byte >= LOWEST_ESCAPABLE_BYTE, "escaped an ASCII byte");
            escaped.push(MARKER);
            escaped.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
            escaped.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
        }
        rest = &invalid[invalid_len..];
    }
}

/// Copies a decodable run through, doubling every marker in it.
fn push_valid(escaped: &mut String, valid: &str) {
    let mut runs = valid.split(MARKER);
    if let Some(first) = runs.next() {
        escaped.push_str(first);
    }
    for run in runs {
        escaped.push(MARKER);
        escaped.push(MARKER);
        escaped.push_str(run);
    }
}

/// Decodes an escaped string back to the exact bytes it was built from.
fn unescape(escaped: &str) -> Result<Vec<u8>, EscapeError> {
    let escaped = escaped.as_bytes();
    let mut bytes = Vec::with_capacity(escaped.len());
    let mut index = 0;
    while index < escaped.len() {
        // Not a marker: copy the byte through. A multi-byte character passes one
        // byte at a time, which is the same bytes in the same order.
        if escaped[index] != MARKER as u8 {
            bytes.push(escaped[index]);
            index += 1;
            continue;
        }
        match escaped.get(index + 1) {
            None => return Err(EscapeError::Truncated),
            Some(&next) if next == MARKER as u8 => {
                bytes.push(MARKER as u8);
                index += 2;
            }
            Some(_) => {
                let digits = escaped
                    .get(index + 1..index + 3)
                    .ok_or(EscapeError::Truncated)?;
                let high = hex_value(digits[0]).ok_or(EscapeError::NotUppercaseHex)?;
                let low = hex_value(digits[1]).ok_or(EscapeError::NotUppercaseHex)?;
                let byte = (high << 4) | low;
                if byte < LOWEST_ESCAPABLE_BYTE {
                    return Err(EscapeError::NotEscapable(byte));
                }
                bytes.push(byte);
                index += 3;
            }
        }
    }
    Ok(bytes)
}

/// Uppercase only, so one file has exactly one escaped spelling.
fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

/// A name or path, escaped so a `String` carries it without losing a byte.
///
/// It is the file's identity, not its label: what crosses the wire, keys maps,
/// and resolves back to the exact bytes. For an ordinary name the two look the
/// same, which is why the type keeps them apart. Every way of getting this wrong —
/// forgetting to decode, escaping twice (`escape` is not idempotent), comparing
/// text a person typed against text off the wire — is silent while the type is
/// `String`, and a compile error while it is this.
///
/// There is deliberately no `Display`, as `std::path::Path` has none:
/// `format!("{path}")` would have to pick one meaning, and either is wrong
/// somewhere. A caller says which — [`PathString::display`] for a person,
/// [`PathString::as_str`] for a program, [`PathString::to_path_buf`] for the
/// filesystem.
///
/// A name is a one-component path, so one type holds both.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PathString(String);

impl PathString {
    /// From bytes the filesystem gave.
    #[must_use]
    pub(crate) fn escape(bytes: &[u8]) -> Self {
        Self(escape(bytes))
    }

    /// From text a person typed, or a literal.
    ///
    /// Valid UTF-8 by construction, so this only has to double the markers.
    #[must_use]
    pub(crate) fn from_literal(text: &str) -> Self {
        Self(escape(text.as_bytes()))
    }

    /// From an escaped string off the wire, refusing one that is not canonical.
    pub(crate) fn from_escaped(escaped: String) -> Result<Self, EscapeError> {
        unescape(&escaped)?;
        Ok(Self(escaped))
    }

    /// From a name or path the filesystem gave.
    #[must_use]
    pub(crate) fn from_os_str(text: &OsStr) -> Self {
        Self::escape(text.as_bytes())
    }

    /// From a real path.
    #[must_use]
    pub(crate) fn from_path(path: &Path) -> Self {
        Self::from_os_str(path.as_os_str())
    }

    /// The escaped form: what crosses the wire, and what paths are compared as.
    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// The escaped form, consuming this, for the wire without a copy.
    #[must_use]
    pub(crate) fn into_string(self) -> String {
        self.0
    }

    /// The exact bytes the filesystem gave.
    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>, EscapeError> {
        unescape(&self.0)
    }

    /// The real path, with nothing expanded: `~` is a directory named `~`.
    pub(crate) fn to_path_buf(&self) -> Result<PathBuf, EscapeError> {
        Ok(PathBuf::from(OsString::from_vec(self.to_bytes()?)))
    }

    /// Appends one name.
    ///
    /// Escaping composes with joining — the escape never emits a `/`, and a name
    /// never holds one — so joining two escaped halves gives the escape of the
    /// joined path, and neither side is decoded. The name is never absolute,
    /// which is why this is concatenation rather than [`Path::join`], and cannot
    /// fail.
    #[must_use]
    pub(crate) fn join(&self, name: &Self) -> Self {
        if self.0.is_empty() {
            return name.clone();
        }
        let separator = if self.0.ends_with('/') { "" } else { "/" };
        Self(format!("{}{separator}{}", self.0, name.0))
    }

    /// For a person to read, and to match a typed query against.
    ///
    /// Decoded, then lossy, so it never round-trips: pass [`PathString::as_str`]
    /// to anything that has to find the file again.
    ///
    /// Borrowed when the escape changed nothing — every name with no marker and
    /// no byte outside UTF-8 — so filtering a directory of ordinary names
    /// allocates nothing here.
    #[must_use]
    pub(crate) fn display(&self) -> Cow<'_, str> {
        if !self.0.contains(MARKER) {
            return Cow::Borrowed(&self.0);
        }
        match self.to_bytes() {
            Ok(bytes) => Cow::Owned(String::from_utf8_lossy(&bytes).into_owned()),
            // Unreachable through the constructors, which all validate.
            Err(_) => Cow::Borrowed(&self.0),
        }
    }

    /// Whether this is one name: a single non-empty component, neither `.` nor
    /// `..` nor absolute — what a destination filename has to be.
    #[must_use]
    pub(crate) fn is_name(&self) -> bool {
        let Ok(path) = self.to_path_buf() else {
            return false;
        };
        let mut components = path.components();
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
    }
}

impl PartialEq<str> for PathString {
    /// Against the escaped form, so an ASCII literal reads naturally: a name with
    /// no marker and no invalid byte escapes to itself.
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for PathString {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

#[cfg(test)]
impl From<&str> for PathString {
    /// Fixtures name entries with literals.
    fn from(text: &str) -> Self {
        Self::from_literal(text)
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt as _, path::Path};

    use super::{EscapeError, LOWEST_ESCAPABLE_BYTE, PathString, escape, unescape};

    /// Round-trips `bytes`, and returns the escaped form so a failure names it.
    fn round_trip(bytes: &[u8]) -> String {
        let escaped = escape(bytes);
        assert_eq!(
            unescape(&escaped).as_deref(),
            Ok(bytes),
            "did not round-trip through {escaped:?}"
        );
        escaped
    }

    #[test]
    fn ordinary_names_escape_to_themselves() {
        for name in ["file.txt", "café.txt", "", "a-b_c.9", "日本語", "..", "."] {
            assert_eq!(round_trip(name.as_bytes()), name);
        }
    }

    #[test]
    fn markers_are_doubled() {
        assert_eq!(round_trip(b"100%.txt"), "100%%.txt");
        assert_eq!(round_trip(b"%"), "%%");
        assert_eq!(round_trip(b"%%"), "%%%%");
        assert_eq!(round_trip(b"a%b%c"), "a%%b%%c");
    }

    #[test]
    fn invalid_bytes_become_uppercase_hex() {
        assert_eq!(round_trip(b"caf\x80.txt"), "caf%80.txt");
        assert_eq!(round_trip(&[0xff]), "%FF");
        // A truncated three-byte sequence: every byte left is a valid prefix of
        // one, so none decodes alone and each is escaped.
        assert_eq!(round_trip(&[0xe0, 0x80]), "%E0%80");
    }

    /// The collision that makes escaping the marker mandatory rather than tidy.
    #[test]
    fn a_literal_escape_never_collides_with_an_escaped_byte() {
        let literal = escape(b"caf%80.txt");
        let invalid = escape(b"caf\x80.txt");
        assert_ne!(literal, invalid);
        assert_eq!(literal, "caf%%80.txt");
        assert_eq!(invalid, "caf%80.txt");
    }

    /// Exhaustive over every one- and two-byte sequence, which covers every lead
    /// byte, every bad continuation, and every truncated sequence.
    #[test]
    fn every_short_byte_sequence_round_trips() {
        for first in 0..=u8::MAX {
            round_trip(&[first]);
            for second in 0..=u8::MAX {
                round_trip(&[first, second]);
            }
        }
    }

    /// Escaping is not idempotent, so escaping twice is a real bug and the type
    /// is what prevents it. Pinned so nobody "fixes" it by making it so.
    #[test]
    fn escaping_is_not_idempotent() {
        let once = escape(b"caf\x80.txt");
        assert_ne!(escape(once.as_bytes()), once);
    }

    #[test]
    fn joining_escaped_halves_is_escaping_the_joined_path() {
        let parent = PathString::escape(b"caf\x80");
        let name = PathString::escape(b"100%.txt");
        assert_eq!(
            parent.join(&name).as_str(),
            escape(b"caf\x80/100%.txt").as_str()
        );
        assert_eq!(PathString::from("/").join(&name), "/100%%.txt");
        assert_eq!(PathString::from("").join(&name), "100%%.txt");
    }

    #[test]
    fn decoding_refuses_a_sequence_that_would_smuggle_in_a_separator() {
        assert_eq!(unescape("a%2Fb"), Err(EscapeError::NotEscapable(b'/')));
        assert_eq!(unescape("a%00b"), Err(EscapeError::NotEscapable(0)));
        assert_eq!(unescape("a%7Fb"), Err(EscapeError::NotEscapable(0x7f)));
        assert_eq!(unescape("a%25b"), Err(EscapeError::NotEscapable(b'%')));
    }

    #[test]
    fn decoding_refuses_a_truncated_or_non_canonical_sequence() {
        assert_eq!(unescape("a%"), Err(EscapeError::Truncated));
        assert_eq!(unescape("a%8"), Err(EscapeError::Truncated));
        assert_eq!(unescape("a%8g"), Err(EscapeError::NotUppercaseHex));
        // Lowercase is refused so one file has one spelling, and therefore one
        // map key and one comparison.
        assert_eq!(unescape("a%ff"), Err(EscapeError::NotUppercaseHex));
        assert_eq!(unescape("a%8f"), Err(EscapeError::NotUppercaseHex));
    }

    #[test]
    fn every_escapable_byte_is_accepted_and_every_other_is_refused() {
        for byte in 0..=u8::MAX {
            let escaped = format!("%{byte:02X}");
            let decoded = unescape(&escaped);
            if byte >= LOWEST_ESCAPABLE_BYTE {
                assert_eq!(decoded, Ok(vec![byte]));
            } else {
                assert_eq!(decoded, Err(EscapeError::NotEscapable(byte)));
            }
        }
    }

    #[test]
    fn a_path_string_reports_the_bytes_it_was_built_from() {
        let path = PathString::escape(b"caf\x80.txt");
        assert_eq!(path.as_str(), "caf%80.txt");
        assert_eq!(path.to_bytes(), Ok(b"caf\x80.txt".to_vec()));
        assert_eq!(path.display(), "caf\u{fffd}.txt");
    }

    #[test]
    fn a_literal_percent_displays_as_one_percent() {
        let path = PathString::from_literal("100%.txt");
        assert_eq!(path.as_str(), "100%%.txt");
        assert_eq!(path.display(), "100%.txt");
    }

    #[test]
    fn from_escaped_validates() {
        assert!(PathString::from_escaped("caf%80.txt".to_owned()).is_ok());
        assert_eq!(
            PathString::from_escaped("caf%2F.txt".to_owned()),
            Err(EscapeError::NotEscapable(b'/'))
        );
    }

    #[test]
    fn a_path_string_converts_to_the_real_path_and_back() {
        let real = Path::new(OsStr::from_bytes(b"/tmp/caf\x80/100%.txt"));
        let path = PathString::from_path(real);
        assert_eq!(path.as_str(), "/tmp/caf%80/100%%.txt");
        assert_eq!(path.to_path_buf().as_deref(), Ok(real));
        assert_eq!(path.into_string(), "/tmp/caf%80/100%%.txt");
    }

    #[test]
    fn a_name_is_told_from_a_path() {
        for name in ["file.txt", "100%%.txt", "caf%80.txt", "~"] {
            assert!(
                PathString::from_escaped(name.to_owned()).unwrap().is_name(),
                "{name} should be a name"
            );
        }
        for name in ["", ".", "..", "/absolute", "nested/name"] {
            assert!(
                !PathString::from_escaped(name.to_owned()).unwrap().is_name(),
                "{name} should not be a name"
            );
        }
    }
}
