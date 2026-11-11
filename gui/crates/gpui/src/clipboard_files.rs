//! Bounded local file-list conversion without filesystem access.

// Rust guideline compliant 2026-10-07
use crate::{ClipboardEntry, ClipboardItem, ClipboardReadError};
use std::path::{Path, PathBuf};
use url::Url;

/// Maximum files in one clipboard list.
pub const MAX_CLIPBOARD_FILES: usize = 256;
/// Maximum encoded native bytes in one clipboard file path.
pub const MAX_CLIPBOARD_FILE_PATH_BYTES: usize = 32 * 1024;
/// Maximum serialized bytes in one file URI-list transfer.
pub const MAX_CLIPBOARD_URI_BYTES: usize = 1024 * 1024;
// A path byte can require three URI bytes; leave room for the file scheme/drive.
const MAX_URI_LINE: usize = MAX_CLIPBOARD_FILE_PATH_BYTES * 3 + 16;

fn validate_path(path: &Path) -> Result<(), ClipboardReadError> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if !path.is_absolute()
        || bytes.len() > MAX_CLIPBOARD_FILE_PATH_BYTES
        || bytes.contains(&0)
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(ClipboardReadError::UnsupportedContent);
    }
    Ok(())
}

/// Parses bounded local file URIs without reading or resolving their paths.
///
/// Accepts LF/CRLF records and comments; rejects remote hosts, credentials,
/// queries, fragments, relative paths, NULs and oversized lists/paths.
///
/// # Errors
/// Returns unsupported content for invalid or excess input.
pub fn parse_file_uri_list(
    value: &str,
    max_bytes: usize,
) -> Result<Vec<PathBuf>, ClipboardReadError> {
    if max_bytes > MAX_CLIPBOARD_URI_BYTES || value.len() > max_bytes {
        return Err(ClipboardReadError::UnsupportedContent);
    }
    let mut paths = Vec::new();
    for line in value.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if paths.len() >= MAX_CLIPBOARD_FILES
            || line.len() > MAX_URI_LINE
            || line.trim() != line
            || line
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace() || byte == b'\\')
            // URL parsing removes dot segments. Reject them first so symlink
            // paths retain their exact meaning instead of being normalized.
            || line.split('/').any(|part| {
                let dots = part.replace("%2e", ".").replace("%2E", ".");
                matches!(dots.as_str(), "." | "..")
            })
            || line
                .split_once(':')
                .is_none_or(|(_, rest)| !rest.starts_with('/'))
        {
            return Err(ClipboardReadError::UnsupportedContent);
        }
        for (index, byte) in line.bytes().enumerate() {
            if byte == b'%'
                && !line
                    .as_bytes()
                    .get(index + 1..index + 3)
                    .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit))
            {
                return Err(ClipboardReadError::UnsupportedContent);
            }
        }
        let uri = Url::parse(line).map_err(|_| ClipboardReadError::UnsupportedContent)?;
        if uri.scheme() != "file"
            || !uri.username().is_empty()
            || uri.password().is_some()
            || uri.port().is_some()
            || uri.query().is_some()
            || uri.fragment().is_some()
            || uri
                .host_str()
                .is_some_and(|host| !host.eq_ignore_ascii_case("localhost"))
        {
            return Err(ClipboardReadError::UnsupportedContent);
        }
        let path = uri
            .to_file_path()
            .map_err(|_| ClipboardReadError::UnsupportedContent)?;
        validate_path(&path)?;
        paths.push(path);
    }
    Ok(paths)
}

fn serialize<'a>(
    paths: impl Iterator<Item = &'a Path>,
    max_bytes: usize,
) -> Result<String, ClipboardReadError> {
    if max_bytes > MAX_CLIPBOARD_URI_BYTES {
        return Err(ClipboardReadError::UnsupportedContent);
    }
    let mut result = String::new();
    for (index, path) in paths.enumerate() {
        if index >= MAX_CLIPBOARD_FILES {
            return Err(ClipboardReadError::UnsupportedContent);
        }
        validate_path(path)?;
        let uri = Url::from_file_path(path).map_err(|_| ClipboardReadError::UnsupportedContent)?;
        if uri
            .host_str()
            .is_some_and(|host| !host.eq_ignore_ascii_case("localhost"))
        {
            return Err(ClipboardReadError::UnsupportedContent);
        }
        let length = result
            .len()
            .checked_add(uri.as_str().len())
            .and_then(|n| n.checked_add(2))
            .filter(|length| *length <= max_bytes)
            .ok_or(ClipboardReadError::UnsupportedContent)?;
        result.reserve(length - result.len());
        result.push_str(uri.as_str());
        result.push_str("\r\n");
    }
    Ok(result)
}

/// Serializes bounded absolute paths as canonical CRLF-separated local file URIs.
///
/// This performs no filesystem I/O and grants no access to file contents.
///
/// # Errors
/// Returns unsupported content for invalid paths or excess files/bytes.
pub fn file_uri_list(paths: &[PathBuf], max_bytes: usize) -> Result<String, ClipboardReadError> {
    serialize(paths.iter().map(PathBuf::as_path), max_bytes)
}

/// Extracts only explicit file entries into a bounded local URI list.
///
/// String/image entries never become file paths. Returns `None` without file
/// entries and preserves an explicitly empty file list as an empty string.
///
/// # Errors
/// Returns unsupported content for more than 64 entries or invalid/excess files.
pub fn clipboard_uri_list(
    item: Option<ClipboardItem>,
    max_bytes: usize,
) -> Result<Option<String>, ClipboardReadError> {
    let Some(item) = item else { return Ok(None) };
    if item.entries.len() > 64 {
        return Err(ClipboardReadError::UnsupportedContent);
    }
    let has_files = item
        .entries
        .iter()
        .any(|entry| matches!(entry, ClipboardEntry::ExternalPaths(_)));
    if !has_files {
        return Ok(None);
    }
    serialize(
        item.entries
            .iter()
            .flat_map(|entry| match entry {
                ClipboardEntry::ExternalPaths(paths) => paths.0.as_slice(),
                _ => &[],
            })
            .map(PathBuf::as_path),
        max_bytes,
    )
    .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_lists_round_trip_spaces_unicode_and_reserved_characters() {
        let paths = vec![
            std::env::temp_dir().join("a #λ?.txt"),
            std::env::temp_dir().join("b\nc"),
        ];
        let encoded = file_uri_list(&paths, MAX_CLIPBOARD_URI_BYTES).unwrap();
        assert!(encoded.contains("%23") && encoded.contains("%0A"));
        assert_eq!(
            parse_file_uri_list(&encoded, MAX_CLIPBOARD_URI_BYTES).unwrap(),
            paths
        );
        let with_comments = format!("# copied files\n{}", encoded.replace("\r\n", "\n"));
        assert_eq!(
            parse_file_uri_list(&with_comments, MAX_CLIPBOARD_URI_BYTES).unwrap(),
            paths
        );
    }
    #[test]
    fn foreign_or_ambiguous_uris_are_rejected_without_partial_results() {
        let valid = file_uri_list(
            &[std::env::temp_dir().join("first")],
            MAX_CLIPBOARD_URI_BYTES,
        )
        .unwrap();
        for bad in [
            "https://example.org/a",
            "file://remote/a",
            "file:///a?secret=x",
            "file:///a#x",
            "file:///a%00b",
            " file:///a",
            "file:///a\t",
            "file:relative",
            "file:///a%zz",
            "file:///a b",
            "file:///a/../b",
            "file:///a/%2e%2E/b",
            "file:///a\\b",
        ] {
            assert!(
                parse_file_uri_list(&format!("{valid}{bad}\r\n"), MAX_CLIPBOARD_URI_BYTES).is_err(),
                "accepted {bad}"
            );
        }
        assert!(file_uri_list(&[PathBuf::from("relative")], MAX_CLIPBOARD_URI_BYTES).is_err());
        assert!(
            file_uri_list(
                &[std::env::temp_dir().join("a/../b")],
                MAX_CLIPBOARD_URI_BYTES
            )
            .is_err()
        );
    }
    #[test]
    fn file_count_path_and_encoded_byte_limits_are_independent() {
        let path = std::env::temp_dir().join("file");
        assert!(
            file_uri_list(
                &vec![path.clone(); MAX_CLIPBOARD_FILES + 1],
                MAX_CLIPBOARD_URI_BYTES
            )
            .is_err()
        );
        let encoded = file_uri_list(&[path], MAX_CLIPBOARD_URI_BYTES).unwrap();
        assert!(
            parse_file_uri_list(
                &encoded.repeat(MAX_CLIPBOARD_FILES + 1),
                MAX_CLIPBOARD_URI_BYTES
            )
            .is_err()
        );
        assert!(parse_file_uri_list(&encoded, encoded.len() - 1).is_err());
        assert!(
            file_uri_list(
                &[std::env::temp_dir().join("x".repeat(MAX_CLIPBOARD_FILE_PATH_BYTES))],
                MAX_CLIPBOARD_URI_BYTES
            )
            .is_err()
        );
    }
    #[test]
    fn text_resembling_a_file_uri_does_not_become_a_file_clipboard() {
        assert_eq!(
            clipboard_uri_list(
                Some(ClipboardItem::new_string("file:///tmp/private".into())),
                MAX_CLIPBOARD_URI_BYTES
            )
            .unwrap(),
            None
        );
    }
    #[cfg(unix)]
    #[test]
    fn native_non_utf8_paths_round_trip_as_percent_encoded_bytes() {
        use std::os::unix::ffi::OsStringExt as _;
        let path = PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/a\xffb".to_vec()));
        let encoded = file_uri_list(std::slice::from_ref(&path), MAX_CLIPBOARD_URI_BYTES).unwrap();
        assert_eq!(
            parse_file_uri_list(&encoded, MAX_CLIPBOARD_URI_BYTES).unwrap(),
            vec![path]
        );
    }
}
