//! Allocation-free single-picture container validation and PNG color metadata.

// Rust guideline compliant 2026-10-07
#[derive(Default)]
pub(super) struct PngColor {
    pub cicp: Option<[u8; 4]>,
    pub srgb: bool,
    pub gamma: Option<u32>,
    pub chromaticities: Option<[u32; 8]>,
}

pub(super) fn png(body: &[u8]) -> Result<PngColor, ()> {
    if !body.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(());
    }
    let mut color = PngColor::default();
    let mut at = 8usize;
    let mut data_seen = false;
    let mut icc_seen = false;
    while at < body.len() {
        let header = body.get(at..at.checked_add(8).ok_or(())?).ok_or(())?;
        let length = u32::from_be_bytes(header[..4].try_into().map_err(|_| ())?) as usize;
        let end = at
            .checked_add(length)
            .and_then(|n| n.checked_add(12))
            .ok_or(())?;
        let chunk = body.get(at + 4..end).ok_or(())?;
        let (payload, crc) = chunk.split_at(chunk.len() - 4);
        if crc32fast::hash(payload) != u32::from_be_bytes(crc.try_into().map_err(|_| ())?) {
            return Err(());
        }
        let data = &payload[4..];
        let kind = &header[4..];
        if matches!(kind, b"acTL" | b"fcTL" | b"fdAT") {
            return Err(());
        }
        if matches!(kind, b"cICP" | b"sRGB" | b"gAMA" | b"cHRM" | b"iCCP") && data_seen {
            return Err(());
        }
        match kind {
            b"cICP" if color.cicp.is_none() => color.cicp = Some(data.try_into().map_err(|_| ())?),
            b"sRGB" if !color.srgb && data.len() == 1 && data[0] <= 3 => color.srgb = true,
            b"gAMA" if color.gamma.is_none() => {
                let gamma = u32::from_be_bytes(data.try_into().map_err(|_| ())?);
                if gamma == 0 {
                    return Err(());
                }
                color.gamma = Some(gamma);
            }
            b"cHRM" if color.chromaticities.is_none() && data.len() == 32 => {
                let mut values = [0; 8];
                for (value, bytes) in values.iter_mut().zip(data.as_chunks::<4>().0) {
                    *value = u32::from_be_bytes(*bytes);
                }
                color.chromaticities = Some(values);
            }
            b"iCCP" if !icc_seen => icc_seen = true,
            b"cICP" | b"sRGB" | b"gAMA" | b"cHRM" | b"iCCP" => return Err(()),
            b"IDAT" => data_seen = true,
            b"IEND" => {
                return if data.is_empty() && data_seen && end == body.len() {
                    Ok(color)
                } else {
                    Err(())
                };
            }
            _ => {}
        }
        at = end;
    }
    Err(())
}

pub(super) fn jpeg(body: &[u8]) -> Result<(), ()> {
    if !body.starts_with(&[0xff, 0xd8]) {
        return Err(());
    }
    let mut at = 2usize;
    let mut in_scan = false;
    let mut scans = 0usize;
    let mut frames = 0usize;
    while at < body.len() {
        if in_scan && body[at] != 0xff {
            at += 1;
            continue;
        }
        if body[at] != 0xff {
            return Err(());
        }
        while body.get(at) == Some(&0xff) {
            at += 1;
        }
        let marker = *body.get(at).ok_or(())?;
        at += 1;
        match marker {
            0x00 | 0xd0..=0xd7 if in_scan => continue,
            0xd9 => {
                return if scans > 0 && frames == 1 && at == body.len() {
                    Ok(())
                } else {
                    Err(())
                };
            }
            0x00 | 0xd0..=0xd8 => return Err(()),
            0x01 => continue,
            _ => {}
        }
        // Progressive scans share one SOF. Hierarchical/multiple frames are
        // outside this static decode profile even without an MPO index.
        if marker == 0xde {
            return Err(());
        }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            frames += 1;
            if frames != 1 {
                return Err(());
            }
        }
        in_scan = false;
        let bytes = body.get(at..at.checked_add(2).ok_or(())?).ok_or(())?;
        let length = u16::from_be_bytes(bytes.try_into().map_err(|_| ())?) as usize;
        if length < 2 {
            return Err(());
        }
        let end = at.checked_add(length).ok_or(())?;
        let data = body.get(at + 2..end).ok_or(())?;
        if marker == 0xe2 && data.starts_with(b"MPF\0") {
            return Err(());
        }
        if marker == 0xda {
            scans += 1;
            if scans > 100 {
                return Err(());
            }
            in_scan = true;
        }
        at = end;
    }
    Err(())
}
