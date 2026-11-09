use std::borrow::Cow;

use crate::config::Encoding;

/// Removes CSV quoting from a raw field. Unquoted fields are returned as-is.
pub(crate) fn unquote<'a>(raw: &'a [u8], scratch: &'a mut Vec<u8>) -> &'a [u8] {
    if raw.len() < 2 || raw[0] != b'"' || raw[raw.len() - 1] != b'"' {
        return raw;
    }
    let inner = &raw[1..raw.len() - 1];
    if !inner.contains(&b'"') {
        return inner;
    }
    scratch.clear();
    let mut i = 0;
    while i < inner.len() {
        scratch.push(inner[i]);
        i += if inner[i] == b'"' { 2 } else { 1 };
    }
    scratch
}

/// Decodes bytes in the input encoding. Returns None for malformed input.
pub(crate) fn decode(encoding: Encoding, bytes: &[u8]) -> Option<Cow<'_, str>> {
    match encoding {
        Encoding::Utf8 => std::str::from_utf8(bytes).ok().map(Cow::Borrowed),
        Encoding::ShiftJis => encoding_rs::SHIFT_JIS.decode_without_bom_handling_and_without_replacement(bytes),
    }
}
