use std::{borrow::Cow, sync::LazyLock};

use anyhow::Result;
use collections::FxHashMap;
use itertools::Itertools;
use windows::Win32::{
    Foundation::{HANDLE, HGLOBAL, HWND},
    System::{
        DataExchange::{
            CloseClipboard, CountClipboardFormats, EmptyClipboard, EnumClipboardFormats,
            GetClipboardData, GetClipboardFormatNameW, IsClipboardFormatAvailable, OpenClipboard,
            RegisterClipboardFormatW, SetClipboardData,
        },
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
        Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT},
    },
    UI::Shell::{DragQueryFileW, HDROP},
};
use windows::core::{Owned, PCWSTR};

use gpui::{
    ClipboardEntry, ClipboardItem, ClipboardString, ExternalPaths, Image, ImageFormat, hash,
};

const DRAGDROP_GET_FILES_COUNT: u32 = 0xFFFFFFFF;

static CLIPBOARD_HASH_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("GPUI internal text hash")));
static CLIPBOARD_METADATA_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("GPUI internal metadata")));
static CLIPBOARD_SVG_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("image/svg+xml")));
static CLIPBOARD_GIF_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("GIF")));
static CLIPBOARD_PNG_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("PNG")));
static CLIPBOARD_JPG_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("JFIF")));

static IMAGE_FORMATS_MAP: LazyLock<FxHashMap<u32, ImageFormat>> = LazyLock::new(|| {
    let mut map = FxHashMap::default();
    map.insert(*CLIPBOARD_PNG_FORMAT, ImageFormat::Png);
    map.insert(*CLIPBOARD_GIF_FORMAT, ImageFormat::Gif);
    map.insert(*CLIPBOARD_JPG_FORMAT, ImageFormat::Jpeg);
    map.insert(*CLIPBOARD_SVG_FORMAT, ImageFormat::Svg);
    map
});

fn register_clipboard_format(format: PCWSTR) -> u32 {
    let ret = unsafe { RegisterClipboardFormatW(format) };
    if ret == 0 {
        panic!(
            "Error when registering clipboard format: {}",
            std::io::Error::last_os_error()
        );
    }
    log::debug!(
        "Registered clipboard format {} as {}",
        unsafe { format.display() },
        ret
    );
    ret
}

fn get_clipboard_data(format: u32) -> Option<LockedGlobal> {
    let global = HGLOBAL(unsafe { GetClipboardData(format).ok() }?.0);
    LockedGlobal::lock(global)
}

pub(crate) fn has_file_list() -> bool {
    // SAFETY: this format query takes no pointers and does not retrieve data.
    unsafe { IsClipboardFormatAvailable(CF_HDROP.0 as u32).is_ok() }
}

// The process clipboard is shared by every window. Keep at most one external
// provider read alive, even if its requesting view drops the returned task.
static READ_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn read_bounded<T: Send + 'static>(
    executor: &gpui::BackgroundExecutor,
    read: impl FnOnce() -> Result<Option<T>, gpui::ClipboardReadError> + Send + 'static,
) -> gpui::Task<Result<Option<T>, gpui::ClipboardReadError>> {
    use std::sync::atomic::Ordering;
    if READ_BUSY.swap(true, Ordering::AcqRel) {
        return gpui::Task::ready(Err(gpui::ClipboardReadError::Unavailable));
    }
    struct Permit;
    impl Drop for Permit {
        fn drop(&mut self) {
            READ_BUSY.store(false, Ordering::Release);
        }
    }
    let permit = Permit;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    executor.spawn(async move {
        let _permit = permit;
        if std::time::Instant::now() >= deadline {
            return Err(gpui::ClipboardReadError::Unavailable);
        }
        let result = read();
        if std::time::Instant::now() >= deadline {
            return Err(gpui::ClipboardReadError::Unavailable);
        }
        result
    })
}

pub(crate) fn read_files_bounded(
    max_bytes: usize,
) -> Result<Option<String>, gpui::ClipboardReadError> {
    let _clip = ClipboardGuard::open_bounded().ok_or(gpui::ClipboardReadError::Unavailable)?;
    file_uris(max_bytes)
}

pub(crate) fn read_text_bounded(
    max_bytes: usize,
) -> Result<Option<String>, gpui::ClipboardReadError> {
    let _clip = ClipboardGuard::open_bounded().ok_or(gpui::ClipboardReadError::Unavailable)?;
    // SAFETY: querying format availability does not dereference application memory.
    if !unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_ok() } {
        return Ok(None);
    }
    let locked =
        get_clipboard_data(CF_UNICODETEXT.0 as u32).ok_or(gpui::ClipboardReadError::Unavailable)?;
    let limit = max_bytes
        .checked_add(1)
        .and_then(|n| n.checked_mul(2))
        .ok_or(gpui::ClipboardReadError::UnsupportedContent)?;
    let bytes = locked.as_bytes();
    let mut words = Vec::new();
    for pair in bytes[..bytes.len().min(limit)].chunks_exact(2) {
        let word = u16::from_le_bytes([pair[0], pair[1]]);
        if word == 0 {
            let text = String::from_utf16(&words)
                .map_err(|_| gpui::ClipboardReadError::UnsupportedContent)?;
            return if text.len() <= max_bytes {
                Ok(Some(text))
            } else {
                Err(gpui::ClipboardReadError::UnsupportedContent)
            };
        }
        words.push(word);
    }
    Err(gpui::ClipboardReadError::UnsupportedContent)
}

fn file_uris(max_bytes: usize) -> Result<Option<String>, gpui::ClipboardReadError> {
    if max_bytes > gpui::MAX_CLIPBOARD_URI_BYTES {
        return Err(gpui::ClipboardReadError::UnsupportedContent);
    }
    if !has_file_list() {
        return Ok(None);
    }
    let locked =
        get_clipboard_data(CF_HDROP.0 as u32).ok_or(gpui::ClipboardReadError::Unavailable)?;
    let paths = parse_dropfiles(locked.as_bytes())?;
    gpui::clipboard_uri_list(
        Some(ClipboardItem {
            entries: vec![ClipboardEntry::ExternalPaths(ExternalPaths(paths.into()))],
        }),
        max_bytes,
    )
}

fn parse_dropfiles(bytes: &[u8]) -> Result<Vec<std::path::PathBuf>, gpui::ClipboardReadError> {
    let invalid = gpui::ClipboardReadError::UnsupportedContent;
    if bytes.len() < 24 || bytes.len() > gpui::MAX_CLIPBOARD_URI_BYTES {
        return Err(invalid);
    }
    let offset = u32::from_le_bytes(bytes[..4].try_into().map_err(|_| invalid.clone())?) as usize;
    // CF_HDROP's wide flag selects UTF-16; reject legacy codepage-dependent offers.
    if offset < 20 || offset > bytes.len() || !offset.is_multiple_of(2) || bytes[16..20] == [0; 4] {
        return Err(invalid);
    }
    let mut paths = Vec::new();
    let mut path = Vec::new();
    let mut terminated = false;
    for pair in bytes[offset..].chunks_exact(2) {
        let word = u16::from_le_bytes([pair[0], pair[1]]);
        if word == 0 {
            if path.is_empty() {
                terminated = true;
                break;
            }
            if paths.len() >= gpui::MAX_CLIPBOARD_FILES {
                return Err(invalid);
            }
            let name = String::from_utf16(&path).map_err(|_| invalid.clone())?;
            paths.push(std::path::PathBuf::from(name));
            path.clear();
        } else {
            if path.len() >= gpui::MAX_CLIPBOARD_FILE_PATH_BYTES / 2 {
                return Err(invalid);
            }
            path.push(word);
        }
    }
    if !terminated {
        return Err(invalid);
    }
    Ok(paths)
}

fn prepare_files(item: &ClipboardItem) -> Result<Option<Vec<u8>>, gpui::ClipboardReadError> {
    let Some(uris) = gpui::clipboard_uri_list(Some(item.clone()), gpui::MAX_CLIPBOARD_URI_BYTES)?
    else {
        return Ok(None);
    };
    let paths = gpui::parse_file_uri_list(&uris, gpui::MAX_CLIPBOARD_URI_BYTES)?;
    let mut bytes = vec![0; 20];
    bytes[..4].copy_from_slice(&20u32.to_le_bytes());
    bytes[16..20].copy_from_slice(&1u32.to_le_bytes());
    for path in paths {
        use std::os::windows::ffi::OsStrExt;
        for word in path.as_os_str().encode_wide().chain(Some(0)) {
            if bytes.len() > gpui::MAX_CLIPBOARD_URI_BYTES - 4 {
                return Err(gpui::ClipboardReadError::UnsupportedContent);
            }
            bytes.extend_from_slice(&word.to_le_bytes());
        }
    }
    if bytes.len() == 20 {
        bytes.extend_from_slice(&[0, 0]);
    }
    bytes.extend_from_slice(&[0, 0]);
    Ok(Some(bytes))
}

pub(crate) fn read_media_bounded()
-> Result<Option<gpui::ClipboardPasteMedia>, gpui::ClipboardReadError> {
    let _clip = ClipboardGuard::open_bounded().ok_or(gpui::ClipboardReadError::Unavailable)?;
    if let Some(files) = file_uris(gpui::MAX_CLIPBOARD_URI_BYTES)? {
        return Ok(Some(gpui::ClipboardPasteMedia::Files(files)));
    }
    for (format, kind) in [
        (*CLIPBOARD_PNG_FORMAT, ImageFormat::Png),
        (*CLIPBOARD_JPG_FORMAT, ImageFormat::Jpeg),
        (CF_DIBV5.0 as u32, ImageFormat::Bmp),
        (CF_DIB.0 as u32, ImageFormat::Bmp),
    ] {
        // SAFETY: format availability has no pointer arguments.
        if !unsafe { IsClipboardFormatAvailable(format).is_ok() } {
            continue;
        }
        let locked = get_clipboard_data(format).ok_or(gpui::ClipboardReadError::Unavailable)?;
        if locked.size > gpui::MAX_CLIPBOARD_IMAGE_BYTES {
            return Err(gpui::ClipboardReadError::UnsupportedContent);
        }
        let bytes = if kind == ImageFormat::Bmp {
            convert_dib_to_bmp(locked.as_bytes())
                .ok_or(gpui::ClipboardReadError::UnsupportedContent)?
        } else {
            locked.as_bytes().to_vec()
        };
        // Release the native clipboard before decoding untrusted pixels.
        drop(locked);
        drop(_clip);
        return gpui::ClipboardPng::from_encoded(kind, &bytes)
            .map(|image| Some(gpui::ClipboardPasteMedia::Image(image)));
    }
    Ok(None)
}

pub(crate) fn write_to_clipboard(item: ClipboardItem, owner: HWND) {
    // Validate the entire file offer before clearing the prior clipboard.
    let files = match prepare_files(&item) {
        Ok(files) => files,
        Err(_) => return,
    };
    let Some(_clip) = ClipboardGuard::open_for(Some(owner)) else {
        return;
    };

    let result: Result<()> = (|| {
        unsafe { EmptyClipboard()? };
        for entry in item.entries() {
            match entry {
                ClipboardEntry::String(string) => write_string(string)?,
                ClipboardEntry::Image(image) => write_image(image)?,
                ClipboardEntry::ExternalPaths(_) => {}
            }
        }
        if let Some(files) = files {
            set_clipboard_bytes(&files, CF_HDROP.0 as u32)?;
        }
        Ok(())
    })();

    if let Err(e) = result {
        log::error!("Failed to write to clipboard: {e}");
    }
}

pub(crate) fn read_from_clipboard() -> Option<ClipboardItem> {
    let _clip = ClipboardGuard::open()?;

    let mut entries = Vec::new();
    let mut have_text = false;
    let mut have_image = false;
    let mut have_files = false;

    let count = unsafe { CountClipboardFormats() };
    let mut format = 0;
    for _ in 0..count {
        format = unsafe { EnumClipboardFormats(format) };

        if !have_text && format == CF_UNICODETEXT.0 as u32 {
            if let Some(entry) = read_string() {
                entries.push(entry);
                have_text = true;
            }
        } else if !have_image && is_image_format(format) {
            if let Some(entry) = read_image(format) {
                entries.push(entry);
                have_image = true;
            }
        } else if !have_files && format == CF_HDROP.0 as u32 {
            if let Some(entry) = read_files() {
                entries.push(entry);
                have_files = true;
            }
        }
    }

    if entries.is_empty() {
        log_unsupported_clipboard_formats();
        return None;
    }
    Some(ClipboardItem { entries })
}

pub(crate) fn with_file_names<F>(hdrop: HDROP, mut f: F)
where
    F: FnMut(String),
{
    let file_count = unsafe { DragQueryFileW(hdrop, DRAGDROP_GET_FILES_COUNT, None) };
    for file_index in 0..file_count {
        let filename_length = unsafe { DragQueryFileW(hdrop, file_index, None) } as usize;
        let mut buffer = vec![0u16; filename_length + 1];
        let ret = unsafe { DragQueryFileW(hdrop, file_index, Some(buffer.as_mut_slice())) };
        if ret == 0 {
            log::error!("unable to read file name of dragged file");
            continue;
        }
        match String::from_utf16(&buffer[0..filename_length]) {
            Ok(file_name) => f(file_name),
            Err(e) => log::error!("dragged file name is not UTF-16: {}", e),
        }
    }
}

fn set_clipboard_bytes<T>(data: &[T], format: u32) -> Result<()> {
    unsafe {
        let global = Owned::new(GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of_val(data))?);
        let ptr = GlobalLock(*global);
        anyhow::ensure!(!ptr.is_null(), "GlobalLock returned null");
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr as _, data.len());
        GlobalUnlock(*global).ok();
        SetClipboardData(format, Some(HANDLE(global.0)))?;
        // SetClipboardData succeeded — the system now owns the memory.
        std::mem::forget(global);
    }
    Ok(())
}

fn get_clipboard_string(format: u32) -> Option<String> {
    let locked = get_clipboard_data(format)?;
    let bytes = locked.as_bytes();
    let words_len = bytes.len() / std::mem::size_of::<u16>();
    if words_len == 0 {
        return Some(String::new());
    }
    let slice = unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const u16, words_len) };
    let actual_len = slice.iter().position(|&c| c == 0).unwrap_or(words_len);
    Some(String::from_utf16_lossy(&slice[..actual_len]))
}

fn is_image_format(format: u32) -> bool {
    IMAGE_FORMATS_MAP.contains_key(&format)
        || format == CF_DIB.0 as u32
        || format == CF_DIBV5.0 as u32
}

fn write_string(item: &ClipboardString) -> Result<()> {
    // CF_UNICODETEXT is null-terminated, so replace embedded NUL characters with spaces.
    let text = if item.text.contains('\0') {
        Cow::Owned(item.text.replace('\0', " "))
    } else {
        Cow::Borrowed(item.text.as_str())
    };
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect_vec();
    set_clipboard_bytes(&wide, CF_UNICODETEXT.0 as u32)?;

    if let Some(metadata) = item.metadata.as_ref() {
        let hash_bytes = ClipboardString::text_hash(&text).to_ne_bytes();
        set_clipboard_bytes(&hash_bytes, *CLIPBOARD_HASH_FORMAT)?;

        let wide: Vec<u16> = metadata.encode_utf16().chain(Some(0)).collect_vec();
        set_clipboard_bytes(&wide, *CLIPBOARD_METADATA_FORMAT)?;
    }
    Ok(())
}

fn write_image(item: &Image) -> Result<()> {
    let native_format = match item.format {
        ImageFormat::Svg => Some(*CLIPBOARD_SVG_FORMAT),
        ImageFormat::Gif => Some(*CLIPBOARD_GIF_FORMAT),
        ImageFormat::Png => Some(*CLIPBOARD_PNG_FORMAT),
        ImageFormat::Jpeg => Some(*CLIPBOARD_JPG_FORMAT),
        _ => None,
    };
    if let Some(format) = native_format {
        set_clipboard_bytes(item.bytes(), format)?;
    }

    if item.format == ImageFormat::Png {
        if let Some(dib) = png_to_dib(item.bytes()) {
            set_clipboard_bytes(&dib, CF_DIBV5.0 as u32)?;
        }
    }

    // Also provide a PNG copy for broad compatibility.
    // SVG can't be rasterized by the image crate, so skip it.
    if item.format != ImageFormat::Svg && native_format != Some(*CLIPBOARD_PNG_FORMAT) {
        if let Some(png_bytes) = convert_to_png(item.bytes(), item.format) {
            set_clipboard_bytes(&png_bytes, *CLIPBOARD_PNG_FORMAT)?;
        }
    }
    Ok(())
}

fn png_to_dib(bytes: &[u8]) -> Option<Vec<u8>> {
    // Reuse the hosted decoder's compressed, geometry and decoded-byte bounds.
    let png = gpui::ClipboardPng::from_encoded(ImageFormat::Png, bytes)
        .ok()?
        .into_bytes();
    let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    let length = image.as_raw().len().checked_add(124)?;
    if length > gpui::MAX_CLIPBOARD_IMAGE_BYTES {
        return None;
    }
    let mut dib = vec![0; 124];
    dib[..4].copy_from_slice(&124u32.to_le_bytes());
    dib[4..8].copy_from_slice(&i32::try_from(image.width()).ok()?.to_le_bytes());
    dib[8..12].copy_from_slice(&(-i32::try_from(image.height()).ok()?).to_le_bytes());
    dib[12..14].copy_from_slice(&1u16.to_le_bytes());
    dib[14..16].copy_from_slice(&32u16.to_le_bytes());
    dib[16..20].copy_from_slice(&3u32.to_le_bytes()); // BI_BITFIELDS
    dib[20..24].copy_from_slice(&u32::try_from(image.as_raw().len()).ok()?.to_le_bytes());
    for (offset, mask) in [
        (40, 0x00ff0000u32),
        (44, 0x0000ff00),
        (48, 0x000000ff),
        (52, 0xff000000),
    ] {
        dib[offset..offset + 4].copy_from_slice(&mask.to_le_bytes());
    }
    dib[56..60].copy_from_slice(&0x73524742u32.to_le_bytes()); // LCS_sRGB
    dib.reserve(image.as_raw().len());
    for pixel in image.as_raw().chunks_exact(4) {
        dib.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
    }
    Some(dib)
}

fn convert_to_png(bytes: &[u8], format: ImageFormat) -> Option<Vec<u8>> {
    let img_format = gpui_to_image_format(format)?;
    let image = image::load_from_memory_with_format(bytes, img_format)
        .map_err(|e| log::warn!("Failed to decode image for PNG conversion: {e}"))
        .ok()?;
    let mut buf = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| log::warn!("Failed to encode PNG: {e}"))
        .ok()?;
    Some(buf)
}

fn read_string() -> Option<ClipboardEntry> {
    let text = get_clipboard_string(CF_UNICODETEXT.0 as u32)?;
    let metadata = read_clipboard_metadata(&text);
    Some(ClipboardEntry::String(ClipboardString { text, metadata }))
}

fn read_clipboard_metadata(text: &str) -> Option<String> {
    let locked = get_clipboard_data(*CLIPBOARD_HASH_FORMAT)?;
    let hash_bytes: [u8; 8] = locked.as_bytes().get(..8)?.try_into().ok()?;
    let hash = u64::from_ne_bytes(hash_bytes);
    if hash != ClipboardString::text_hash(text) {
        return None;
    }
    get_clipboard_string(*CLIPBOARD_METADATA_FORMAT)
}

fn read_image(format: u32) -> Option<ClipboardEntry> {
    let locked = get_clipboard_data(format)?;
    let (bytes, image_format) = if format == CF_DIB.0 as u32 || format == CF_DIBV5.0 as u32 {
        (convert_dib_to_bmp(locked.as_bytes())?, ImageFormat::Bmp)
    } else {
        let image_format = *IMAGE_FORMATS_MAP.get(&format)?;
        (locked.as_bytes().to_vec(), image_format)
    };
    let id = hash(&bytes);
    Some(ClipboardEntry::Image(Image {
        format: image_format,
        bytes,
        id,
    }))
}

fn read_files() -> Option<ClipboardEntry> {
    let locked = get_clipboard_data(CF_HDROP.0 as u32)?;
    let filenames = parse_dropfiles(locked.as_bytes()).ok()?;
    Some(ClipboardEntry::ExternalPaths(ExternalPaths(
        filenames.into(),
    )))
}

/// DIB is BMP without the 14-byte BITMAPFILEHEADER. Prepend one.
fn convert_dib_to_bmp(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 || dib.len() > gpui::MAX_CLIPBOARD_IMAGE_BYTES - 14 {
        return None;
    }

    let header_size = u32::from_le_bytes(dib[0..4].try_into().ok()?);
    let bit_count = u16::from_le_bytes(dib[14..16].try_into().ok()?);
    let compression = u32::from_le_bytes(dib[16..20].try_into().ok()?);
    if !matches!(header_size, 40 | 52 | 56 | 108 | 124)
        || header_size as usize > dib.len()
        || !matches!(bit_count, 1 | 4 | 8 | 16 | 24 | 32)
        || !matches!(compression, 0 | 3 | 6)
    {
        return None;
    }

    let color_table_size = if bit_count <= 8 {
        let colors_used = u32::from_le_bytes(dib[32..36].try_into().ok()?);
        if colors_used > (1u32 << bit_count) {
            return None;
        }
        (if colors_used == 0 {
            1u32 << bit_count
        } else {
            colors_used
        })
        .checked_mul(4)?
    } else if header_size == 40 && matches!(compression, 3 | 6) {
        if compression == 6 { 16 } else { 12 }
    } else {
        0
    };

    let pixel_offset = 14u32
        .checked_add(header_size)?
        .checked_add(color_table_size)?;
    let file_size = 14u32.checked_add(u32::try_from(dib.len()).ok()?)?;
    if pixel_offset >= file_size {
        return None;
    }

    let mut bmp = Vec::with_capacity(file_size as usize);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&[0u8; 4]); // reserved
    bmp.extend_from_slice(&pixel_offset.to_le_bytes());
    bmp.extend_from_slice(dib);
    Some(bmp)
}

fn log_unsupported_clipboard_formats() {
    let count = unsafe { CountClipboardFormats() };
    let mut format = 0;
    for _ in 0..count {
        format = unsafe { EnumClipboardFormats(format) };
        let mut buffer = [0u16; 64];
        unsafe { GetClipboardFormatNameW(format, &mut buffer) };
        let format_name = String::from_utf16_lossy(&buffer);
        log::warn!(
            "Try to paste with unsupported clipboard format: {}, {}.",
            format,
            format_name
        );
    }
}

fn gpui_to_image_format(value: ImageFormat) -> Option<image::ImageFormat> {
    match value {
        ImageFormat::Png => Some(image::ImageFormat::Png),
        ImageFormat::Jpeg => Some(image::ImageFormat::Jpeg),
        ImageFormat::Webp => Some(image::ImageFormat::WebP),
        ImageFormat::Gif => Some(image::ImageFormat::Gif),
        ImageFormat::Bmp => Some(image::ImageFormat::Bmp),
        ImageFormat::Tiff => Some(image::ImageFormat::Tiff),
        other => {
            log::warn!("No image crate equivalent for format: {other:?}");
            None
        }
    }
}

struct ClipboardGuard;

impl ClipboardGuard {
    // Only background bounded reads use this retry. Clipboard viewers may
    // briefly own the clipboard immediately after receiving a change notice.
    fn open_bounded() -> Option<Self> {
        for attempt in 0..20 {
            // SAFETY: a read needs no owner window. Each success is closed by Drop.
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Some(Self);
            }
            if attempt < 19 {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        None
    }

    fn open() -> Option<Self> {
        Self::open_for(None)
    }

    fn open_for(owner: Option<HWND>) -> Option<Self> {
        // SAFETY: the caller keeps its platform window live through the write.
        // Reads may use no owner; writes must supply one before EmptyClipboard.
        match unsafe { OpenClipboard(owner) } {
            Ok(()) => Some(Self),
            Err(e) => {
                log::error!("Failed to open clipboard: {e}");
                None
            }
        }
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        if let Err(e) = unsafe { CloseClipboard() } {
            log::error!("Failed to close clipboard: {e}");
        }
    }
}

struct LockedGlobal {
    global: HGLOBAL,
    ptr: *const u8,
    size: usize,
}

impl LockedGlobal {
    fn lock(global: HGLOBAL) -> Option<Self> {
        let size = unsafe { GlobalSize(global) };
        if size == 0 || size > isize::MAX as usize {
            return None;
        }
        let ptr = unsafe { GlobalLock(global) };
        if ptr.is_null() {
            return None;
        }
        Some(Self {
            global,
            ptr: ptr as *const u8,
            size,
        })
    }

    fn as_bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.size) }
    }
}

impl Drop for LockedGlobal {
    fn drop(&mut self) {
        unsafe { GlobalUnlock(self.global).ok() };
    }
}

#[cfg(test)]
mod bounded_tests {
    use super::*;

    fn files(paths: &[&str]) -> ClipboardItem {
        ClipboardItem {
            entries: vec![ClipboardEntry::ExternalPaths(ExternalPaths(
                paths.iter().map(std::path::PathBuf::from).collect(),
            ))],
        }
    }

    #[test]
    fn file_offer_round_trip_and_invalid_paths() {
        assert!(
            parse_dropfiles(&prepare_files(&files(&[])).unwrap().unwrap())
                .unwrap()
                .is_empty()
        );
        let item = files(&[r"C:\Users\Public\é 文.png", r"D:\second.txt"]);
        let bytes = prepare_files(&item).unwrap().unwrap();
        assert_eq!(
            parse_dropfiles(&bytes).unwrap(),
            [
                std::path::PathBuf::from(r"C:\Users\Public\é 文.png"),
                std::path::PathBuf::from(r"D:\second.txt"),
            ]
        );
        assert!(prepare_files(&files(&[r"C:\valid.txt", "relative"])).is_err());
        assert!(prepare_files(&files(&[r"\\server\share\file"])).is_err());
        assert!(
            prepare_files(&ClipboardItem::new_string("C:\\plain.txt".into()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rejects_malformed_and_excess_dropfiles() {
        let bytes = prepare_files(&files(&[r"C:\valid.txt"])).unwrap().unwrap();
        for end in 0..bytes.len() {
            assert!(parse_dropfiles(&bytes[..end]).is_err());
        }
        let mut bad = bytes.clone();
        bad[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_dropfiles(&bad).is_err());
        let mut bad = bytes;
        bad[16..20].fill(0);
        assert!(parse_dropfiles(&bad).is_err());
        assert!(prepare_files(&files(&vec![r"C:\a"; gpui::MAX_CLIPBOARD_FILES + 1])).is_err());
    }

    #[test]
    fn dib_pixels_are_normalized_and_bad_geometry_is_rejected() {
        let mut dib = vec![0; 44];
        dib[..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&1i32.to_le_bytes());
        dib[8..12].copy_from_slice(&1i32.to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&24u16.to_le_bytes());
        dib[40..44].copy_from_slice(&[3, 2, 1, 0]);
        let bmp = convert_dib_to_bmp(&dib).unwrap();
        let png = gpui::ClipboardPng::from_encoded(ImageFormat::Bmp, &bmp)
            .unwrap()
            .into_bytes();
        let native = png_to_dib(&png).unwrap();
        let copy = convert_dib_to_bmp(&native).unwrap();
        assert_eq!(
            image::load_from_memory(&copy).unwrap().to_rgba8().as_raw(),
            &[1, 2, 3, 255]
        );
        assert_eq!(
            image::load_from_memory(&png).unwrap().to_rgba8().as_raw(),
            &[1, 2, 3, 255]
        );
        dib[4..8].copy_from_slice(&8193i32.to_le_bytes());
        assert!(
            gpui::ClipboardPng::from_encoded(ImageFormat::Bmp, &convert_dib_to_bmp(&dib).unwrap())
                .is_err()
        );
        dib[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(convert_dib_to_bmp(&dib).is_none());
    }

    #[test]
    fn v5_masks_are_inside_the_header() {
        let mut dib = vec![0; 128];
        dib[..4].copy_from_slice(&124u32.to_le_bytes());
        dib[14..16].copy_from_slice(&32u16.to_le_bytes());
        dib[16..20].copy_from_slice(&3u32.to_le_bytes());
        let bmp = convert_dib_to_bmp(&dib).unwrap();
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 138);
    }
}
