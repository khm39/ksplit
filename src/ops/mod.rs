pub(crate) mod inspect;
pub(crate) mod profile;
pub(crate) mod split;

use crate::config::{CsvDialect, Encoding, Input, KeySpec};
use crate::error::{ConfigError, Error};
use crate::key::{decode, unquote, KeyExtractor};
use crate::source::CsvSource;

/// Opens the input and resolves the key column against the header.
pub(crate) fn open_with_key(
    input: Input,
    dialect: &CsvDialect,
    encoding: Encoding,
    key: &KeySpec,
) -> Result<(CsvSource, KeyExtractor), Error> {
    let source = CsvSource::open(input, dialect)?;
    let names = header_names(&source, encoding)?;
    let column = key.selector.resolve(names.as_deref())?;
    let extractor = KeyExtractor::new(column, key.key_type.clone(), key.trim, encoding);
    Ok((source, extractor))
}

pub(crate) fn header_names(
    source: &CsvSource,
    encoding: Encoding,
) -> Result<Option<Vec<String>>, Error> {
    let Some(fields) = source.header_fields() else {
        return Ok(None);
    };
    let mut scratch = Vec::new();
    let mut names = Vec::with_capacity(fields.len());
    for raw in fields {
        let bytes = unquote(raw, &mut scratch);
        let name = decode(encoding, bytes).ok_or_else(|| {
            ConfigError::Other("header row is not valid in the input encoding".into())
        })?;
        names.push(name.into_owned());
    }
    Ok(Some(names))
}
