use std::path::Path;

use encoding_rs::GBK;

use super::{
    AdapterInput, AdapterOutput, DocumentFormat, DocumentMetadata, FormatAdapter, LoadError,
    LoadWarning, plain_document,
};

pub(super) struct TxtAdapter;

impl FormatAdapter for TxtAdapter {
    fn format(&self) -> DocumentFormat { DocumentFormat::Txt }

    fn probe(&self, path: &Path, prefix: &[u8]) -> u8 {
        if prefix.starts_with(b"PK\x03\x04") {
            return 0;
        }
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
        {
            return 90;
        }
        if std::str::from_utf8(prefix.strip_prefix(b"\xef\xbb\xbf").unwrap_or(prefix)).is_ok() {
            return 40;
        }
        10
    }

    fn load(&self, input: AdapterInput<'_>) -> Result<AdapterOutput, LoadError> {
        let (decoded, used_gb18030) = decode(input.bytes)?;
        let text = normalise_newlines(&decoded);
        if text.len() > input.limits.max_text_bytes {
            return Err(LoadError::TextTooLarge { limit: input.limits.max_text_bytes });
        }
        if looks_binary(&text) {
            return Err(LoadError::UnsupportedFormat(input.path.to_path_buf()));
        }

        let title =
            input.path.file_stem().and_then(|name| name.to_str()).unwrap_or("Untitled").to_owned();
        let metadata =
            DocumentMetadata { title: Some(title.clone()), ..DocumentMetadata::default() };
        let warnings = if used_gb18030 {
            vec![LoadWarning::new(
                "txt.gb18030",
                "TXT was decoded as GB18030 because it was not valid UTF-8",
            )]
        } else {
            Vec::new()
        };

        Ok(AdapterOutput { document: plain_document(text, metadata, title), warnings })
    }
}

fn decode(bytes: &[u8]) -> Result<(String, bool), LoadError> {
    let without_bom = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    if let Ok(text) = std::str::from_utf8(without_bom) {
        return Ok((text.to_owned(), false));
    }

    let (text, _, had_errors) = GBK.decode(bytes);
    if had_errors { Err(LoadError::InvalidTextEncoding) } else { Ok((text.into_owned(), true)) }
}

fn normalise_newlines(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn looks_binary(text: &str) -> bool {
    if text.contains('\0') {
        return true;
    }
    let sample = text.chars().take(4096);
    let mut total = 0_usize;
    let mut suspicious = 0_usize;
    for character in sample {
        total += 1;
        if character.is_control() && !matches!(character, '\n' | '\r' | '\t') {
            suspicious += 1;
        }
    }
    total > 0 && suspicious * 20 > total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_all_common_newlines() {
        assert_eq!(normalise_newlines("a\r\nb\rc\n"), "a\nb\nc\n");
    }

    #[test]
    fn decodes_utf8_bom() {
        let (text, fallback) = decode(b"\xef\xbb\xbfhello").unwrap();
        assert_eq!(text, "hello");
        assert!(!fallback);
    }

    #[test]
    fn decodes_gb18030_fallback() {
        let (bytes, _, had_errors) = GBK.encode("第一章");
        assert!(!had_errors);
        let (text, fallback) = decode(&bytes).unwrap();
        assert_eq!(text, "第一章");
        assert!(fallback);
    }
}
