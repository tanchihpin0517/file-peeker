//! Selected-host names and paths, held as valid UTF-8 without losing a byte.
//!
//! A Unix filename is any byte sequence except `/` and NUL, so a name need not be
//! UTF-8 — while a protobuf `string` and a Rust `String` both must be. This module
//! is the bridge: [`HostPath`] holds an escaped form that is always valid UTF-8 and
//! always decodes back to the exact bytes the host gave us.
//!
//! The escape has to escape its own marker, and that is forced rather than chosen.
//! Escaping only the invalid bytes is not invertible: a file really can be named
//! `caf%80.txt` in plain ASCII, and it would encode to itself and collide with the
//! file whose name is `caf` followed by the byte `0x80`. Nor is there a marker
//! character to reserve instead — every valid Rust `String` is a possible filename,
//! so an injective encoding must move some already-valid names off themselves.
//!
//! The tempting alternative is unavailable here. Python's `surrogateescape` and
//! Windows' `OsString` map an invalid byte `b` to `U+DC00 + b`, which needs no
//! marker at all because a lone surrogate cannot occur in valid UTF-8 — but Rust's
//! `char` excludes `U+D800`–`U+DFFF`, so a `String` cannot hold one. That encoding
//! is WTF-8, which is exactly why `OsString` on Windows is not a `String`.
//!
//! | Input | Escaped |
//! | --- | --- |
//! | a valid UTF-8 run | verbatim |
//! | the literal `%` | `%%` |
//! | a byte that is not valid UTF-8 | `%XX`, uppercase hex |
//!
//! Two properties earn their keep elsewhere in the crate:
//!
//! - **The escaped form never contains `/`,** and `/` cannot occur inside a name,
//!   so `escape(join(a, b)) == join(escape(a), escape(b))`. That is what lets
//!   `browser`'s `child_path`, `Path::join` and `Path::starts_with` keep operating
//!   on escaped paths with no change at all.
//! - **The escaped form is canonical,** because decoding accepts only uppercase hex.
//!   Paths are compared and used as map keys as escaped strings, so two spellings of
//!   one file would be two different keys.

// The slim server build reaches only part of this: displaying a name and comparing
// one against a literal belong to the front ends, which that build leaves out.
#![cfg_attr(not(feature = "client"), allow(dead_code))]

use std::{
    borrow::Cow,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use thiserror::Error;

/// Marks an escape sequence, and is itself escaped by doubling.
const MARKER: char = '%';

const HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

/// The lowest byte an escape may carry. Every byte below this is valid UTF-8 on its
/// own, so [`escape`] never emits one — and refusing them on the way back is what
/// stops a decoded `%2F` from introducing a path separator, or `%00` a NUL, inside
/// what is meant to be a single name.
const LOWEST_ESCAPABLE_BYTE: u8 = 0x80;

/// Why an escaped string could not be decoded.
///
/// Only reachable from data that did not come out of [`escape`], which in practice
/// means a peer that is not speaking this protocol.
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

/// Escapes host bytes into valid UTF-8.
#[must_use]
pub(crate) fn escape(bytes: &[u8]) -> String {
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
        // that is left is a valid prefix of one and none of it decodes alone.
        let invalid_len = error.error_len().unwrap_or(invalid.len());
        for &byte in &invalid[..invalid_len] {
            // The first byte that fails to decode is never ASCII -- an ASCII byte
            // always decodes -- and neither a lead byte nor a continuation byte is.
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
pub(crate) fn unescape(escaped: &str) -> Result<Vec<u8>, EscapeError> {
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

/// A selected-host name or path, escaped so a `String` carries it losslessly.
///
/// The newtype is the point. Every way of getting this wrong — forgetting to decode,
/// escaping something twice (`escape` is not idempotent), comparing text a person
/// typed against text off the wire — is silent while the type is `String`, and a
/// compile error while it is this.
///
/// There is deliberately no `Display`: [`HostPath::display`] is lossy and must not be
/// reachable from `format!` by accident. What crosses the wire is
/// [`HostPath::as_escaped`].
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct HostPath(String);

impl HostPath {
    /// From bytes the owning host gave us.
    #[must_use]
    pub(crate) fn escape(bytes: &[u8]) -> Self {
        Self(escape(bytes))
    }

    /// From text a person typed, or an ASCII literal in a test.
    ///
    /// Valid UTF-8 by construction, so this only has to double the markers.
    #[must_use]
    pub(crate) fn from_literal(text: &str) -> Self {
        Self(escape(text.as_bytes()))
    }

    /// From an escaped string off the wire, rejecting one that is not canonical.
    pub(crate) fn from_escaped(escaped: String) -> Result<Self, EscapeError> {
        unescape(&escaped)?;
        Ok(Self(escaped))
    }

    /// A component, or a leading run of components, taken from a path that is
    /// already escaped.
    ///
    /// Infallible, and safely so: the escape never emits a `/`, so every `%XX` lies
    /// wholly inside one component and slicing an escaped path at a separator cannot
    /// cut an escape in half. That is what lets a breadcrumb split a path into
    /// segments that each stay addressable.
    #[must_use]
    pub(crate) fn from_escaped_fragment(escaped: String) -> Self {
        debug_assert!(
            unescape(&escaped).is_ok(),
            "fragment of an escaped path is not itself escaped: {escaped:?}"
        );
        Self(escaped)
    }

    /// Holds an escaped form without checking it, so a test can drive what a peer
    /// that is not speaking this protocol would send. The real constructors all
    /// validate, which is why this is the only way to build one that does not.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_unvalidated_for_test(escaped: &str) -> Self {
        Self(escaped.to_owned())
    }

    /// A real path on the host that owns it.
    #[cfg(unix)]
    pub(crate) fn to_path_buf(&self) -> Result<PathBuf, EscapeError> {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt as _};

        Ok(PathBuf::from(OsString::from_vec(self.to_bytes()?)))
    }

    /// A real path on the host that owns it.
    ///
    /// Off Unix an `OsStr` is not bytes, so a name that is not UTF-8 has nothing to
    /// convert into. This project has never supported such a host — there is no
    /// `cfg(windows)` anywhere in the crate and CI builds Linux and macOS — so the
    /// arm reports rather than guesses.
    #[cfg(not(unix))]
    pub(crate) fn to_path_buf(&self) -> Result<PathBuf, EscapeError> {
        let bytes = self.to_bytes()?;
        String::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|error| EscapeError::NotEscapable(error.as_bytes()[0]))
    }

    /// Escapes a name or path the owning host gave us.
    #[must_use]
    #[cfg(unix)]
    pub(crate) fn escape_os(text: &OsStr) -> Self {
        use std::os::unix::ffi::OsStrExt as _;

        Self::escape(text.as_bytes())
    }

    /// Escapes a name or path the owning host gave us.
    ///
    /// See [`HostPath::to_path_buf`] for why the non-Unix arm differs: with no bytes
    /// to read, all it can do is take the lossy form, which is exact for text that
    /// is valid Unicode and approximate for text that is not.
    #[must_use]
    #[cfg(not(unix))]
    pub(crate) fn escape_os(text: &OsStr) -> Self {
        Self::escape(text.to_string_lossy().as_bytes())
    }

    /// Escapes a real path on the host that owns it.
    #[must_use]
    pub(crate) fn from_path(path: &Path) -> Self {
        Self::escape_os(path.as_os_str())
    }

    /// The escaped form: what crosses the wire, and what paths are compared as.
    #[must_use]
    pub(crate) fn as_escaped(&self) -> &str {
        &self.0
    }

    /// The escaped form, consuming this, for putting on the wire without a copy.
    #[must_use]
    pub(crate) fn into_escaped(self) -> String {
        self.0
    }

    /// Appends one path component.
    ///
    /// Escaping composes with joining — the escape never emits a `/`, and a name can
    /// never contain one — so joining two escaped halves gives the escape of the
    /// joined path, and neither side has to be decoded to build a child path. The
    /// component is a single name, never absolute, which is why this can be plain
    /// concatenation instead of [`Path::join`] and therefore cannot fail.
    #[must_use]
    pub(crate) fn join(&self, component: &Self) -> Self {
        if self.0.is_empty() {
            return component.clone();
        }
        let separator = if self.0.ends_with('/') { "" } else { "/" };
        Self(format!("{}{separator}{}", self.0, component.0))
    }

    /// The exact bytes the owning host gave us.
    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>, EscapeError> {
        unescape(&self.0)
    }

    /// For a person to read, and to match a typed query against.
    ///
    /// Decoded and then lossy, so it never round-trips — pass
    /// [`HostPath::as_escaped`] to anything that has to address the file again.
    ///
    /// Borrowed when the escape changed nothing, which is every name with no marker
    /// and no byte outside UTF-8 — so filtering a directory of ordinary filenames
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

    /// Whether this is a single non-empty component, neither `.` nor `..` nor
    /// absolute — what a destination filename has to be.
    #[must_use]
    pub(crate) fn is_one_component(&self) -> bool {
        let Ok(path) = self.to_path_buf() else {
            return false;
        };
        let mut components = path.components();
        matches!(components.next(), Some(std::path::Component::Normal(_)))
            && components.next().is_none()
    }
}

impl PartialEq<str> for HostPath {
    /// Against the escaped form, so an ASCII literal in a test reads naturally: a
    /// name with no marker and no invalid byte escapes to itself.
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for HostPath {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

#[cfg(test)]
impl From<&str> for HostPath {
    /// Fixtures name entries with ASCII literals, which are already their own
    /// escaped form.
    fn from(text: &str) -> Self {
        Self::from_literal(text)
    }
}

#[cfg(test)]
mod tests {
    use super::{EscapeError, HostPath, LOWEST_ESCAPABLE_BYTE, escape, unescape};

    /// Round-trips `bytes`, and reports the escaped form so a failure names it.
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
        // A truncated three-byte sequence: every byte left is a valid prefix of one,
        // so none of them decodes alone and each is escaped.
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

    /// Escaping is not idempotent, so double-escaping is a real bug and the type is
    /// what prevents it. Pinned so nobody "fixes" it by making it so.
    #[test]
    fn escaping_is_not_idempotent() {
        let once = escape(b"caf\x80.txt");
        assert_ne!(escape(once.as_bytes()), once);
    }

    #[test]
    fn escaped_output_never_holds_a_separator_or_a_marker_run_it_did_not_mean() {
        // Joining escaped halves is joining the escape of the join, which is what
        // lets the browser keep doing path arithmetic on escaped paths.
        let parent = escape(b"caf\x80");
        let name = escape(b"100%.txt");
        assert_eq!(format!("{parent}/{name}"), escape(b"caf\x80/100%.txt"));
    }

    #[test]
    fn decoding_rejects_a_sequence_that_would_smuggle_in_a_separator() {
        assert_eq!(unescape("a%2Fb"), Err(EscapeError::NotEscapable(b'/')));
        assert_eq!(unescape("a%00b"), Err(EscapeError::NotEscapable(0)));
        assert_eq!(unescape("a%7Fb"), Err(EscapeError::NotEscapable(0x7f)));
        assert_eq!(unescape("a%25b"), Err(EscapeError::NotEscapable(b'%')));
    }

    #[test]
    fn decoding_rejects_a_truncated_or_non_canonical_sequence() {
        assert_eq!(unescape("a%"), Err(EscapeError::Truncated));
        assert_eq!(unescape("a%8"), Err(EscapeError::Truncated));
        assert_eq!(unescape("a%8g"), Err(EscapeError::NotUppercaseHex));
        // Lowercase is refused so one file has one escaped spelling, and therefore
        // one map key and one path comparison.
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
    fn a_host_path_reports_the_bytes_it_was_built_from() {
        let path = HostPath::escape(b"caf\x80.txt");
        assert_eq!(path.as_escaped(), "caf%80.txt");
        assert_eq!(path.to_bytes(), Ok(b"caf\x80.txt".to_vec()));
        assert_eq!(path.display(), "caf\u{fffd}.txt");
    }

    #[test]
    fn a_literal_percent_displays_as_one_percent() {
        let path = HostPath::from_literal("100%.txt");
        assert_eq!(path.as_escaped(), "100%%.txt");
        assert_eq!(path.display(), "100%.txt");
    }

    #[test]
    fn from_escaped_validates() {
        assert!(HostPath::from_escaped("caf%80.txt".to_owned()).is_ok());
        assert_eq!(
            HostPath::from_escaped("caf%2F.txt".to_owned()),
            Err(EscapeError::NotEscapable(b'/'))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_host_path_converts_to_the_real_path_and_back() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt as _, path::Path};

        let real = Path::new(OsStr::from_bytes(b"/tmp/caf\x80/100%.txt"));
        let host = HostPath::from_path(real);
        assert_eq!(host.as_escaped(), "/tmp/caf%80/100%%.txt");
        assert_eq!(host.to_path_buf().as_deref(), Ok(real));
    }

    #[test]
    fn one_component_names_are_told_from_paths() {
        for name in ["file.txt", "100%%.txt", "caf%80.txt"] {
            assert!(
                HostPath::from_escaped(name.to_owned())
                    .unwrap()
                    .is_one_component(),
                "{name} should be one component"
            );
        }
        for name in ["", ".", "..", "/absolute", "nested/name"] {
            assert!(
                !HostPath::from_escaped(name.to_owned())
                    .unwrap()
                    .is_one_component(),
                "{name} should not be one component"
            );
        }
    }
}
