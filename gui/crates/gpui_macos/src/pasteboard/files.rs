//! Bounded local file URL metadata on the native pasteboard.

// Rust guideline compliant 2026-11-19
use super::Pasteboard;
use gpui::{ClipboardReadError, MAX_CLIPBOARD_FILES, MAX_CLIPBOARD_URI_BYTES};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::NSPasteboardItem;
use objc2_foundation::{NSArray, NSData, ns_string};
use std::path::PathBuf;

impl Pasteboard {
    pub(crate) fn has_file_urls(&self) -> bool {
        self.inner
            .availableTypeFromArray(&NSArray::from_slice(&[ns_string!("public.file-url")]))
            .is_some()
    }

    pub(crate) fn read_file_uris_bounded(
        &self,
        max_bytes: usize,
    ) -> Result<Option<String>, ClipboardReadError> {
        use ClipboardReadError::{Unavailable, UnsupportedContent};
        if max_bytes > MAX_CLIPBOARD_URI_BYTES {
            return Err(UnsupportedContent);
        }
        let revision = self.inner.changeCount();
        if !self.has_file_urls() {
            return Ok(None);
        }
        let items = self.inner.pasteboardItems().ok_or(Unavailable)?;
        if items.len() > MAX_CLIPBOARD_FILES {
            return Err(UnsupportedContent);
        }
        let mut list = String::new();
        for item in &items {
            let kind = ns_string!("public.file-url");
            if !item.types().containsObject(kind) {
                continue;
            }
            let data = item.dataForType(kind).ok_or(Unavailable)?;
            if data
                .len()
                .checked_add(2)
                .is_none_or(|len| len > max_bytes - list.len())
            {
                return Err(UnsupportedContent);
            }
            let url = String::from_utf8(data.to_vec()).map_err(|_| UnsupportedContent)?;
            if url.is_empty() || url.starts_with('#') || url.contains(['\r', '\n']) {
                return Err(UnsupportedContent);
            }
            list.push_str(&url);
            list.push_str("\r\n");
        }
        if self.inner.changeCount() != revision || list.is_empty() {
            return Err(Unavailable);
        }
        let paths = gpui::parse_file_uri_list(&list, max_bytes)?;
        gpui::file_uri_list(&paths, max_bytes).map(Some)
    }

    pub(super) fn write_files(&self, paths: &[PathBuf]) {
        let Ok(list) = gpui::file_uri_list(paths, MAX_CLIPBOARD_URI_BYTES) else {
            return;
        };
        let mut items = Vec::with_capacity(paths.len());
        for url in list.lines() {
            let item = NSPasteboardItem::new();
            if !item.setData_forType(
                &NSData::with_bytes(url.as_bytes()),
                ns_string!("public.file-url"),
            ) {
                return;
            }
            items.push(ProtocolObject::from_retained(item));
        }
        let items = NSArray::from_retained_slice(&items);
        self.inner.clearContents();
        if !items.is_empty() {
            self.inner.writeObjects(&items);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{ClipboardEntry, ClipboardItem, ExternalPaths};

    #[test]
    fn file_urls_round_trip_through_native_items_and_plain_text_stays_text() {
        let board = Pasteboard::unique();
        let paths = [
            PathBuf::from("/private/tmp/fixture 雪 one"),
            PathBuf::from("/private/tmp/a#b"),
        ];
        let expected = gpui::file_uri_list(&paths, MAX_CLIPBOARD_URI_BYTES).unwrap();
        board.write(ClipboardItem {
            entries: vec![ClipboardEntry::ExternalPaths(ExternalPaths(
                paths.clone().into_iter().collect(),
            ))],
        });
        assert!(board.has_file_urls());
        assert_eq!(
            board.read_file_uris_bounded(expected.len()).unwrap(),
            Some(expected.clone())
        );
        assert!(board.read_file_uris_bounded(expected.len() - 1).is_err());
        assert!(
            matches!(&board.read().unwrap().entries[0], ClipboardEntry::ExternalPaths(p) if p.paths() == paths)
        );
        board.write_files(&[PathBuf::from("relative")]);
        assert_eq!(
            board.read_file_uris_bounded(expected.len()).unwrap(),
            Some(expected.clone())
        );
        board.write(ClipboardItem::new_string(expected));
        assert!(!board.has_file_urls());
        assert_eq!(board.read_file_uris_bounded(128).unwrap(), None);
    }

    #[test]
    fn reject_an_invalid_native_file_offer_as_a_whole() {
        let board = Pasteboard::unique();
        for bad in [
            "file://remote/share",
            "file:///one\nfile:///two",
            "file:///bad%xx",
        ] {
            let mut items = Vec::new();
            for url in ["file:///valid", bad] {
                let item = NSPasteboardItem::new();
                assert!(item.setData_forType(
                    &NSData::with_bytes(url.as_bytes()),
                    ns_string!("public.file-url")
                ));
                items.push(ProtocolObject::from_retained(item));
            }
            board.inner.clearContents();
            assert!(
                board
                    .inner
                    .writeObjects(&NSArray::from_retained_slice(&items))
            );
            assert!(
                board
                    .read_file_uris_bounded(MAX_CLIPBOARD_URI_BYTES)
                    .is_err()
            );
        }
    }
}
