use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};

use filedescriptor::Pipe;
use strum::IntoEnumIterator;
use wayland_client::{Connection, protocol::wl_data_offer::WlDataOffer};
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1;

use crate::linux::platform::{PIPE_READ_TIMEOUT, read_fd_with_timeout};
use gpui::{ClipboardEntry, ClipboardItem, Image, ImageFormat, hash};

/// Text mime types that we'll offer to other programs.
pub(crate) const TEXT_MIME_TYPES: [&str; 3] =
    ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];
pub(crate) const FILE_LIST_MIME_TYPE: &str = "text/uri-list";

/// Text mime types that we'll accept from other programs.
pub(crate) const ALLOWED_TEXT_MIME_TYPES: [&str; 2] = ["text/plain;charset=utf-8", "UTF8_STRING"];

pub(crate) struct Clipboard {
    connection: Connection,
    writes: crate::linux::clipboard_write::ClipboardWrites,
    self_mime: String,

    source: Option<wayland_client::protocol::wl_data_source::WlDataSource>,

    // Internal clipboard
    contents: Option<ClipboardItem>,
    primary_contents: Option<ClipboardItem>,

    // External clipboard
    cached_read: Option<ClipboardItem>,
    current_offer: Option<DataOffer<WlDataOffer>>,
    cached_primary_read: Option<ClipboardItem>,
    current_primary_offer: Option<DataOffer<ZwpPrimarySelectionOfferV1>>,
}

pub(crate) trait ReceiveData {
    fn receive_data(&self, mime_type: String, fd: BorrowedFd<'_>);
}

impl ReceiveData for WlDataOffer {
    fn receive_data(&self, mime_type: String, fd: BorrowedFd<'_>) {
        self.receive(mime_type, fd);
    }
}

impl ReceiveData for ZwpPrimarySelectionOfferV1 {
    fn receive_data(&self, mime_type: String, fd: BorrowedFd<'_>) {
        self.receive(mime_type, fd);
    }
}

#[derive(Clone, Debug)]
/// Wrapper for `WlDataOffer` and `ZwpPrimarySelectionOfferV1`, used to help track mime types.
pub(crate) struct DataOffer<T: ReceiveData> {
    pub inner: T,
    mime_types: Vec<String>,
}

impl<T: ReceiveData> DataOffer<T> {
    fn prepare_media(
        &self,
        connection: &Connection,
    ) -> Result<
        crate::linux::clipboard_text::ClipboardRead<gpui::ClipboardPasteMedia>,
        gpui::ClipboardReadError,
    > {
        use std::os::fd::AsFd as _;
        if self.has_mime_type(FILE_LIST_MIME_TYPE) {
            let read = self.prepare_uri_list(connection, gpui::MAX_CLIPBOARD_URI_BYTES)?;
            return Ok(Box::new(move |deadline| {
                Ok(read(deadline)?.map(gpui::ClipboardPasteMedia::Files))
            }));
        }
        let Some(format) = [ImageFormat::Png, ImageFormat::Tiff, ImageFormat::Jpeg]
            .into_iter()
            .find(|format| self.has_mime_type(format.mime_type()))
        else {
            return Ok(Box::new(|_| Ok(None)));
        };
        let pipe = Pipe::new().map_err(|_| gpui::ClipboardReadError::Unavailable)?;
        self.inner
            .receive_data(format.mime_type().into(), pipe.write.as_fd());
        connection
            .flush()
            .map_err(|_| gpui::ClipboardReadError::Unavailable)?;
        drop(pipe.write);
        Ok(Box::new(move |deadline| {
            let bytes = crate::linux::clipboard_text::read_bytes(
                pipe.read,
                deadline,
                gpui::MAX_CLIPBOARD_IMAGE_BYTES,
            )?;
            let png = gpui::ClipboardPng::from_encoded(format, &bytes)?;
            Ok(Some(gpui::ClipboardPasteMedia::Image(png)))
        }))
    }
    fn prepare_uri_list(
        &self,
        connection: &Connection,
        max_bytes: usize,
    ) -> Result<crate::linux::clipboard_text::ClipboardTextRead, gpui::ClipboardReadError> {
        use std::os::fd::AsFd as _;
        if !self.has_mime_type(FILE_LIST_MIME_TYPE) {
            return Err(gpui::ClipboardReadError::UnsupportedContent);
        }
        let pipe = Pipe::new().map_err(|_| gpui::ClipboardReadError::Unavailable)?;
        self.inner
            .receive_data(FILE_LIST_MIME_TYPE.into(), pipe.write.as_fd());
        connection
            .flush()
            .map_err(|_| gpui::ClipboardReadError::Unavailable)?;
        drop(pipe.write);
        Ok(Box::new(move |deadline| {
            crate::linux::clipboard_text::read_pipe(pipe.read, deadline, max_bytes)?
                .map(|text| {
                    gpui::parse_file_uri_list(&text, max_bytes)
                        .and_then(|paths| gpui::file_uri_list(&paths, max_bytes))
                })
                .transpose()
        }))
    }
    fn prepare_text(
        &self,
        connection: &Connection,
        max_bytes: usize,
    ) -> Result<crate::linux::clipboard_text::ClipboardTextRead, gpui::ClipboardReadError> {
        use std::os::fd::AsFd as _;
        let Some(mime) = self
            .mime_types
            .iter()
            .find(|mime| ALLOWED_TEXT_MIME_TYPES.contains(&mime.as_str()))
        else {
            return Err(gpui::ClipboardReadError::UnsupportedContent);
        };
        let pipe = Pipe::new().map_err(|_| gpui::ClipboardReadError::Unavailable)?;
        self.inner.receive_data(mime.clone(), pipe.write.as_fd());
        connection
            .flush()
            .map_err(|_| gpui::ClipboardReadError::Unavailable)?;
        drop(pipe.write);
        Ok(Box::new(move |deadline| {
            crate::linux::clipboard_text::read_pipe(pipe.read, deadline, max_bytes)
        }))
    }
    pub fn new(offer: T) -> Self {
        Self {
            inner: offer,
            mime_types: Vec::new(),
        }
    }

    pub fn add_mime_type(&mut self, mime_type: String) {
        // Match the nested SDK frontend's bounded MIME metadata contract.
        if self.mime_types.len() < 32
            && !mime_type.is_empty()
            && mime_type.len() <= 128
            && mime_type.is_ascii()
            && !mime_type.bytes().any(|byte| byte.is_ascii_control())
            && !self.has_mime_type(&mime_type)
        {
            self.mime_types.push(mime_type);
        }
    }

    fn has_mime_type(&self, mime_type: &str) -> bool {
        self.mime_types.iter().any(|t| t == mime_type)
    }

    fn read_bytes(&self, connection: &Connection, mime_type: &str) -> Option<Vec<u8>> {
        let pipe = Pipe::new().unwrap();
        self.inner.receive_data(mime_type.to_string(), unsafe {
            BorrowedFd::borrow_raw(pipe.write.as_raw_fd())
        });
        let fd = pipe.read;
        drop(pipe.write);

        connection.flush().unwrap();

        match read_fd_with_timeout(fd, PIPE_READ_TIMEOUT) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                log::error!("error reading clipboard pipe: {err:?}");
                None
            }
        }
    }

    fn read_text(&self, connection: &Connection) -> Option<ClipboardItem> {
        let mime_type = self.mime_types.iter().find(|&mime_type| {
            ALLOWED_TEXT_MIME_TYPES
                .iter()
                .any(|&allowed| allowed == mime_type)
        })?;
        let bytes = self.read_bytes(connection, mime_type)?;
        let text_content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(e) => {
                log::error!("Failed to convert clipboard content to UTF-8: {}", e);
                return None;
            }
        };

        // Normalize the text to unix line endings, otherwise
        // copying from eg: firefox inserts a lot of blank
        // lines, and that is super annoying.
        let result = text_content.replace("\r\n", "\n");
        Some(ClipboardItem::new_string(result))
    }

    fn read_image(&self, connection: &Connection) -> Option<ClipboardItem> {
        for format in ImageFormat::iter() {
            let mime_type = format.mime_type();
            if !self.has_mime_type(mime_type) {
                continue;
            }

            if let Some(bytes) = self.read_bytes(connection, mime_type) {
                let id = hash(&bytes);
                return Some(ClipboardItem {
                    entries: vec![ClipboardEntry::Image(Image { format, bytes, id })],
                });
            }
        }
        None
    }
}

impl Clipboard {
    pub(crate) fn prepare_media(
        &self,
    ) -> Result<
        crate::linux::clipboard_text::ClipboardRead<gpui::ClipboardPasteMedia>,
        gpui::ClipboardReadError,
    > {
        match &self.current_offer {
            Some(offer) => offer.prepare_media(&self.connection),
            None => Ok(Box::new(|_| Ok(None))),
        }
    }
    pub(crate) fn has_file_list(&self) -> bool {
        self.current_offer
            .as_ref()
            .is_some_and(|offer| offer.has_mime_type(FILE_LIST_MIME_TYPE))
    }
    pub(crate) fn prepare_uri_list(
        &self,
        max_bytes: usize,
    ) -> Result<crate::linux::clipboard_text::ClipboardTextRead, gpui::ClipboardReadError> {
        let Some(offer) = &self.current_offer else {
            return Ok(Box::new(|_| Ok(None)));
        };
        if offer.has_mime_type(&self.self_mime) {
            let files = gpui::clipboard_uri_list(self.contents.clone(), max_bytes);
            Ok(Box::new(move |_| files))
        } else {
            offer.prepare_uri_list(&self.connection, max_bytes)
        }
    }
    pub(crate) fn prepare_text(
        &self,
        primary: bool,
        max_bytes: usize,
    ) -> Result<crate::linux::clipboard_text::ClipboardTextRead, gpui::ClipboardReadError> {
        fn prepare<T: ReceiveData>(
            offer: Option<&DataOffer<T>>,
            contents: &Option<ClipboardItem>,
            own_mime: &str,
            connection: &Connection,
            max_bytes: usize,
        ) -> Result<crate::linux::clipboard_text::ClipboardTextRead, gpui::ClipboardReadError>
        {
            let Some(offer) = offer else {
                return Ok(Box::new(|_| Ok(None)));
            };
            if offer.has_mime_type(own_mime) {
                let text = gpui::clipboard_text(contents.clone(), max_bytes);
                Ok(Box::new(move |_| text))
            } else {
                offer.prepare_text(connection, max_bytes)
            }
        }
        if primary {
            prepare(
                self.current_primary_offer.as_ref(),
                &self.primary_contents,
                &self.self_mime,
                &self.connection,
                max_bytes,
            )
        } else {
            prepare(
                self.current_offer.as_ref(),
                &self.contents,
                &self.self_mime,
                &self.connection,
                max_bytes,
            )
        }
    }
    pub fn new(connection: Connection) -> Self {
        Self {
            connection,
            writes: Default::default(),
            self_mime: format!("pid/{}", std::process::id()),

            source: None,
            contents: None,
            primary_contents: None,

            cached_read: None,
            current_offer: None,
            cached_primary_read: None,
            current_primary_offer: None,
        }
    }

    pub fn set(
        &mut self,
        item: ClipboardItem,
        source: wayland_client::protocol::wl_data_source::WlDataSource,
    ) {
        self.source = Some(source);
        self.contents = Some(item);
    }

    pub fn set_primary(&mut self, item: ClipboardItem) {
        self.primary_contents = Some(item);
    }

    pub fn set_offer(&mut self, data_offer: Option<DataOffer<WlDataOffer>>) {
        self.cached_read = None;
        self.current_offer = data_offer;
    }

    pub fn set_primary_offer(&mut self, data_offer: Option<DataOffer<ZwpPrimarySelectionOfferV1>>) {
        self.cached_primary_read = None;
        self.current_primary_offer = data_offer;
    }

    pub fn self_mime(&self) -> String {
        self.self_mime.clone()
    }

    pub fn send(
        &self,
        source: &wayland_client::protocol::wl_data_source::WlDataSource,
        mime_type: String,
        fd: OwnedFd,
    ) {
        if self.source.as_ref() != Some(source) {
            return;
        }
        if let Some([ClipboardEntry::Image(image)]) =
            self.contents.as_ref().map(ClipboardItem::entries)
        {
            if matches!(
                image.format,
                ImageFormat::Png | ImageFormat::Tiff | ImageFormat::Jpeg
            ) && image.format.mime_type() == mime_type
            {
                self.writes.send_image(fd, &image.bytes);
            }
            return;
        }
        let text = if mime_type == FILE_LIST_MIME_TYPE {
            gpui::clipboard_uri_list(self.contents.clone(), gpui::MAX_CLIPBOARD_URI_BYTES)
                .ok()
                .flatten()
        } else if TEXT_MIME_TYPES.contains(&mime_type.as_str()) {
            gpui::clipboard_text(self.contents.clone(), 1024 * 1024)
                .ok()
                .flatten()
        } else {
            None
        };
        if let Some(text) = text {
            self.send_bytes(fd, text.as_bytes().to_owned());
        }
    }

    pub fn send_primary(&self, mime_type: String, fd: OwnedFd) {
        if TEXT_MIME_TYPES.contains(&mime_type.as_str())
            && let Some(text) = gpui::clipboard_text(self.primary_contents.clone(), 1024 * 1024)
                .ok()
                .flatten()
        {
            self.send_bytes(fd, text.as_bytes().to_owned());
        }
    }

    pub fn read(&mut self) -> Option<ClipboardItem> {
        let offer = self.current_offer.as_ref()?;
        if let Some(cached) = self.cached_read.clone() {
            return Some(cached);
        }

        if offer.has_mime_type(&self.self_mime) {
            return self.contents.clone();
        }

        let item = offer
            .read_text(&self.connection)
            .or_else(|| offer.read_image(&self.connection))?;

        self.cached_read = Some(item.clone());
        Some(item)
    }

    pub fn read_primary(&mut self) -> Option<ClipboardItem> {
        let offer = self.current_primary_offer.as_ref()?;
        if let Some(cached) = self.cached_primary_read.clone() {
            return Some(cached);
        }

        if offer.has_mime_type(&self.self_mime) {
            return self.primary_contents.clone();
        }

        let item = offer
            .read_text(&self.connection)
            .or_else(|| offer.read_image(&self.connection))?;

        self.cached_primary_read = Some(item.clone());
        Some(item)
    }

    pub fn send_bytes(&self, fd: OwnedFd, bytes: Vec<u8>) {
        self.writes.send(fd, bytes);
    }
}

#[cfg(test)]
mod file_list_tests {
    use super::*;
    use std::{
        cell::RefCell,
        fs::File,
        io::Write as _,
        os::unix::net::UnixStream,
        time::{Duration, Instant},
    };

    struct Source {
        bytes: &'static [u8],
        held: RefCell<Option<OwnedFd>>,
        stall: bool,
        mime: &'static str,
    }
    impl ReceiveData for Source {
        fn receive_data(&self, mime_type: String, fd: BorrowedFd<'_>) {
            assert_eq!(mime_type, self.mime);
            let owned = fd.try_clone_to_owned().unwrap();
            if self.stall {
                self.held.replace(Some(owned));
            } else {
                File::from(owned).write_all(self.bytes).unwrap();
            }
        }
    }

    #[test]
    fn offer_metadata_is_bounded_before_retention() {
        let mut offer = DataOffer::new(Source {
            bytes: b"",
            held: RefCell::new(None),
            stall: false,
            mime: FILE_LIST_MIME_TYPE,
        });
        for bad in [
            String::new(),
            "x".repeat(129),
            "text/λ".into(),
            "text/plain\n".into(),
        ] {
            offer.add_mime_type(bad);
        }
        assert!(offer.mime_types.is_empty());
        offer.add_mime_type(FILE_LIST_MIME_TYPE.into());
        for n in 0..1000 {
            offer.add_mime_type(FILE_LIST_MIME_TYPE.into());
            offer.add_mime_type(format!("application/x-{n}"));
        }
        assert_eq!(offer.mime_types.len(), 32);
        assert!(offer.has_mime_type(FILE_LIST_MIME_TYPE));
    }

    #[test]
    fn media_reads_only_the_selected_representation() {
        const FIXTURE_PNG: &[u8] = &[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 244, 34, 127, 138, 0, 0, 0, 14, 73, 68, 65, 84, 120, 156, 99, 248, 207,
            192, 0, 66, 245, 0, 15, 121, 3, 125, 21, 103, 11, 0, 0, 0, 0, 0, 73, 69, 78, 68, 174,
            66, 96, 130,
        ];
        let (client, _server) = UnixStream::pair().unwrap();
        let connection = Connection::from_socket(client).unwrap();
        let read = |mime, bytes, formats: &[&str]| {
            let mut offer = DataOffer::new(Source {
                mime,
                bytes,
                held: RefCell::new(None),
                stall: false,
            });
            for format in formats {
                offer.add_mime_type((*format).into());
            }
            offer.prepare_media(&connection).unwrap()(Instant::now() + Duration::from_secs(1))
        };
        let Some(gpui::ClipboardPasteMedia::Image(image)) =
            read("image/png", FIXTURE_PNG, &["image/png"]).unwrap()
        else {
            panic!("missing image")
        };
        assert_eq!(
            image.into_bytes(),
            gpui::ClipboardPng::from_encoded(ImageFormat::Png, FIXTURE_PNG)
                .unwrap()
                .into_bytes()
        );
        assert!(matches!(
            read(
                FILE_LIST_MIME_TYPE,
                b"file:///tmp/example",
                &["image/png", FILE_LIST_MIME_TYPE]
            ),
            Ok(Some(gpui::ClipboardPasteMedia::Files(_)))
        ));
        assert!(read("image/png", b"broken", &["image/png"]).is_err());
        assert!(matches!(read("", b"", &["text/plain"]), Ok(None)));
    }

    #[test]
    fn external_file_offers_are_typed_bounded_validated_and_timed() {
        let (client, _server) = UnixStream::pair().unwrap();
        let connection = Connection::from_socket(client).unwrap();
        for (bytes, max, stall, expected) in [
            (
                &b"# files\nfile:///tmp/a%20b\n"[..],
                128,
                false,
                Some("file:///tmp/a%20b\r\n"),
            ),
            (&b"file://remote/a\n"[..], 128, false, None),
            (&b"file:///tmp/a\n"[..], 3, false, None),
            (&b"\xff"[..], 128, false, None),
            (&b""[..], 128, true, None),
        ] {
            let mut offer = DataOffer::new(Source {
                bytes,
                stall,
                mime: FILE_LIST_MIME_TYPE,
                held: RefCell::new(None),
            });
            assert!(offer.prepare_uri_list(&connection, max).is_err());
            offer.add_mime_type(FILE_LIST_MIME_TYPE.into());
            let read = offer.prepare_uri_list(&connection, max).unwrap();
            let start = Instant::now();
            let actual = read(start + Duration::from_millis(25));
            if let Some(expected) = expected {
                assert_eq!(actual.unwrap().as_deref(), Some(expected));
            } else {
                assert!(actual.is_err());
            }
            assert!(start.elapsed() < Duration::from_secs(1));
        }
    }
}
