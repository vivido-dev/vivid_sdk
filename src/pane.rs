//! Stateful helpers for presenting one image from a terminal pane.

use std::fmt;
use std::io;

use vivid_protocol::surface::SurfaceDescriptor;

use crate::*;

const DEFAULT_MAX_COLUMNS: u32 = 80;
const DEFAULT_MAX_ROWS: u32 = 24;
const FIXED_ONE: i64 = 1_i64 << 32;

/// Placement and descriptor options for one pane image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneImageOptions {
    pub title: String,
    pub columns: Option<u32>,
    pub rows: Option<u32>,
    pub text_layer: u64,
}

impl Default for PaneImageOptions {
    fn default() -> Self {
        Self {
            title: "image".into(),
            columns: None,
            rows: None,
            text_layer: 1,
        }
    }
}

/// A Vivid producer session that owns at most one pane-scoped image presentation.
///
/// Creating a new presentation clears the previous node and surface. The inherited
/// `VIVID_ENDPOINT_*` and `VIVID_ROOT_SECRET` environment are consumed only by
/// [`PaneSession::from_env`]; no capability material is retained in this type's debug output.
pub struct PaneSession {
    session: Session,
    current: Option<PanePresentation>,
    next_frame_id: u64,
}

struct PanePresentation {
    context_id: u64,
    node_id: u64,
    surface: Surface,
    channel: TrackChannel,
}

impl fmt::Debug for PaneSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PaneSession")
            .field("session_id", &self.session.info().session_id)
            .field("has_presentation", &self.current.is_some())
            .finish()
    }
}

impl PaneSession {
    /// Connect using the standard Vivid discovery and authentication environment.
    pub fn from_env() -> io::Result<Self> {
        Self::from_session(Session::connect(ProducerConfig::default())?)
    }

    /// Wrap an existing producer session, including an offline session used by tests.
    pub fn from_session(session: Session) -> io::Result<Self> {
        if !session.supports(TERMINAL_SURFACE) {
            return Err(input_error(
                "pane presentation requires the terminal-surface-v1 profile",
            ));
        }
        Ok(Self {
            session,
            current: None,
            next_frame_id: 1,
        })
    }

    /// Present a complete PNG or JPEG with default placement.
    pub fn show_encoded_image(&mut self, encoded: &[u8]) -> io::Result<()> {
        self.show_encoded_image_with_options(encoded, &PaneImageOptions::default())
    }

    /// Present a complete PNG or JPEG with explicit descriptor and placement options.
    pub fn show_encoded_image_with_options(
        &mut self,
        encoded: &[u8],
        options: &PaneImageOptions,
    ) -> io::Result<()> {
        let (encoding, width, height) = encoded_image_info(encoded)?;
        validate_dimensions(width, height)?;
        let encoded_length = u32::try_from(encoded.len())
            .map_err(|_| input_error("encoded image exceeds the Vivid record limit"))?;
        let bits = u64::from(encoded_length)
            .checked_mul(8)
            .ok_or_else(|| input_error("encoded image resource claim overflows u64"))?;
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| input_error("image pixel charge overflows u64"))?;
        let kind = KindConfiguration::EncodedImage(ImageConfiguration {
            encoding,
            width,
            height,
            encoded_length,
            sha256: None,
            cache_lookup: false,
        });
        self.present(
            width,
            height,
            options,
            4,
            encoded_length,
            bits,
            1,
            pixels,
            kind,
            |channel, _| channel.send_image(encoded).map(|_| ()),
        )
    }

    /// Present one complete, tightly packed sRGB RGBA8 frame with default placement.
    pub fn show_rgba(&mut self, width: u32, height: u32, rgba: &[u8]) -> io::Result<()> {
        self.show_rgba_with_options(width, height, rgba, &PaneImageOptions::default())
    }

    /// Present one complete, tightly packed sRGB RGBA8 frame with explicit options.
    pub fn show_rgba_with_options(
        &mut self,
        width: u32,
        height: u32,
        rgba: &[u8],
        options: &PaneImageOptions,
    ) -> io::Result<()> {
        validate_dimensions(width, height)?;
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| input_error("raster pixel charge overflows u64"))?;
        let expected = pixels
            .checked_mul(4)
            .ok_or_else(|| input_error("raster byte length overflows u64"))?;
        if u64::try_from(rgba.len()).ok() != Some(expected) {
            return Err(input_error(
                "RGBA input length does not equal width * height * 4",
            ));
        }
        let maximum_record_body = u32::try_from(
            expected
                .checked_add(72)
                .ok_or_else(|| input_error("raster record body overflows u64"))?,
        )
        .map_err(|_| input_error("raster record body exceeds the Vivid record limit"))?;
        let bits = u64::from(maximum_record_body)
            .checked_mul(8)
            .ok_or_else(|| input_error("raster resource claim overflows u64"))?;
        let kind = KindConfiguration::Raster(RasterConfiguration {
            width,
            height,
            alpha_mode: 1,
            delta_enabled: false,
            maximum_delta_operations: 1,
            zstd_enabled: false,
        });
        let frame_id = self.next_frame_id;
        self.next_frame_id = self
            .next_frame_id
            .checked_add(1)
            .ok_or_else(|| input_error("pane frame identity space exhausted"))?;
        self.present(
            width,
            height,
            options,
            3,
            maximum_record_body,
            bits,
            1,
            pixels,
            kind,
            |channel, _| channel.send_raster(0, frame_id, rgba, false).map(|_| ()),
        )
    }

    /// Remove the current scene node and destroy its complete owner-scoped surface.
    pub fn clear(&mut self) -> io::Result<()> {
        let Some(presentation) = self.current.take() else {
            return Ok(());
        };
        let mut first_error = presentation.channel.close().err();
        if let Err(error) = self.session.delete_node(
            presentation.context_id,
            presentation.node_id,
            &RequestMetadata::default(),
        ) {
            first_error.get_or_insert(error);
        }
        if let Err(error) = self
            .session
            .destroy_surface(&presentation.surface, &RequestMetadata::default())
        {
            first_error.get_or_insert(error);
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Clear the presentation and close the underlying producer session.
    pub fn close(mut self) -> io::Result<()> {
        let clear = self.clear();
        let close = self.session.close();
        clear.and(close)
    }

    #[allow(clippy::too_many_arguments)]
    fn present<F>(
        &mut self,
        width: u32,
        height: u32,
        options: &PaneImageOptions,
        slot: u64,
        maximum_record_body: u32,
        maximum_encoded_bits_per_second: u64,
        maximum_records_per_second: u64,
        retained_pixel_charge: u64,
        kind: KindConfiguration,
        send: F,
    ) -> io::Result<()>
    where
        F: FnOnce(&TrackChannel, &Track) -> io::Result<()>,
    {
        validate_options(options)?;
        self.clear()?;

        let context_id = self.session.info().root_context_id;
        let surface_id = self.session.allocate_id()?;
        let node_id = self.session.allocate_id()?;
        let surface = self.session.create_surface(
            SurfaceDefinition {
                context_id,
                surface_id,
                semantic_profile: GENERIC_CONTENT.into(),
                coordinate_model: CoordinateModel::DesktopLogicalPixels,
                logical_width: u64::from(width),
                logical_height: u64::from(height),
                scale_numerator: 1,
                scale_denominator: 1,
                rotation: 0,
                descriptor: SurfaceDescriptor {
                    role: SurfaceRole::Figure,
                    title: options.title.clone(),
                    semantic_content_revision: 1,
                    semantic_availability: 0,
                    locator_hint: String::new(),
                },
                policy: 0,
                profile_parameters: vec![],
            },
            &RequestMetadata::default(),
        )?;

        let result = (|| {
            let columns = options.columns.unwrap_or(width.min(DEFAULT_MAX_COLUMNS));
            let rows = options.rows.unwrap_or(height.min(DEFAULT_MAX_ROWS));
            self.session.place_terminal_surface(
                &surface,
                node_id,
                0,
                0,
                fixed_cells(columns)?,
                fixed_cells(rows)?,
                options.text_layer,
            )?;
            let track = self.session.create_track(
                TrackConfiguration {
                    direction: Default::default(),
                    context_id,
                    surface_id,
                    track_id: self.session.allocate_id()?,
                    slot,
                    mode: TrackMode::Live,
                    lane: LaneClass::Bulk,
                    maximum_record_body,
                    maximum_rate_millihertz: 1,
                    maximum_encoded_bits_per_second,
                    maximum_records_per_second,
                    maximum_inflight_body_bytes: u64::from(maximum_record_body),
                    kind,
                    target_latency_us: 0,
                    maximum_latency_us: 0,
                    retained_pixel_charge,
                },
                &RequestMetadata::default(),
            )?;
            let channel = self.session.open_track_channel(&track)?;
            send(&channel, &track)?;
            self.session.activate_tracks(
                &surface,
                &[SlotBinding {
                    slot,
                    track_id: track.id(),
                    expected_channel_generation: track.channel_generation(),
                    required_milestone: MILESTONE_OUTPUT_READY,
                }],
                &RequestMetadata::default(),
            )?;
            Ok(PanePresentation {
                context_id,
                node_id,
                surface: surface.clone(),
                channel,
            })
        })();

        match result {
            Ok(presentation) => {
                self.current = Some(presentation);
                Ok(())
            }
            Err(error) => {
                let _ = self
                    .session
                    .delete_node(context_id, node_id, &RequestMetadata::default());
                let _ = self
                    .session
                    .destroy_surface(&surface, &RequestMetadata::default());
                Err(error)
            }
        }
    }
}

fn fixed_cells(cells: u32) -> io::Result<i64> {
    if cells == 0 {
        return Err(input_error("pane placement dimensions must be nonzero"));
    }
    i64::from(cells)
        .checked_mul(FIXED_ONE)
        .ok_or_else(|| input_error("pane placement exceeds signed 32.32 geometry"))
}

fn validate_dimensions(width: u32, height: u32) -> io::Result<()> {
    if width == 0 || height == 0 {
        Err(input_error("pane image dimensions must be nonzero"))
    } else {
        Ok(())
    }
}

fn validate_options(options: &PaneImageOptions) -> io::Result<()> {
    if options.columns == Some(0) || options.rows == Some(0) {
        return Err(input_error("pane placement dimensions must be nonzero"));
    }
    if options.text_layer > 2 {
        return Err(input_error("pane text layer must be in 0..=2"));
    }
    Ok(())
}

fn encoded_image_info(data: &[u8]) -> io::Result<(u64, u32, u32)> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") && data.len() >= 24 {
        let width = u32::from_be_bytes(data[16..20].try_into().expect("fixed PNG width"));
        let height = u32::from_be_bytes(data[20..24].try_into().expect("fixed PNG height"));
        return Ok((1, width, height));
    }
    if data.starts_with(&[0xff, 0xd8]) {
        let mut offset = 2_usize;
        while offset.checked_add(4).is_some_and(|end| end <= data.len()) {
            if data[offset] != 0xff {
                return Err(input_error("invalid JPEG marker stream"));
            }
            let marker = data[offset + 1];
            offset += 2;
            if matches!(marker, 0xd8 | 0xd9) {
                continue;
            }
            let length = usize::from(u16::from_be_bytes([data[offset], data[offset + 1]]));
            if length < 2
                || offset
                    .checked_add(length)
                    .is_none_or(|end| end > data.len())
            {
                return Err(input_error("truncated JPEG segment"));
            }
            if (0xc0..=0xc3).contains(&marker) {
                if length < 7 {
                    return Err(input_error("invalid JPEG frame header"));
                }
                let height = u32::from(u16::from_be_bytes([data[offset + 3], data[offset + 4]]));
                let width = u32::from(u16::from_be_bytes([data[offset + 5], data[offset + 6]]));
                return Ok((2, width, height));
            }
            offset += length;
        }
    }
    Err(input_error(
        "only complete PNG and JPEG images are supported",
    ))
}

fn input_error(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_2x1() -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&2_u32.to_be_bytes());
        png.extend_from_slice(&1_u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        png
    }

    #[test]
    fn pane_session_replaces_and_clears_owner_scoped_presentations() {
        let session = Session::connect(ProducerConfig::offline()).unwrap();
        let mut pane = PaneSession::from_session(session).unwrap();
        pane.show_encoded_image(&png_2x1()).unwrap();
        pane.show_rgba(1, 1, &[1, 2, 3, 4]).unwrap();
        pane.clear().unwrap();
        pane.clear().unwrap();
        assert!(format!("{pane:?}").contains("has_presentation: false"));
    }

    #[test]
    fn pane_session_rejects_bad_media_before_mutation() {
        let session = Session::connect(ProducerConfig::offline()).unwrap();
        let mut pane = PaneSession::from_session(session).unwrap();
        pane.show_rgba(1, 1, &[0; 4]).unwrap();
        assert_eq!(
            pane.show_encoded_image(b"not an image").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            pane.show_rgba(2, 2, &[0; 4]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(format!("{pane:?}").contains("has_presentation: true"));
    }

    #[test]
    fn pane_debug_contains_no_authentication_material() {
        let session = Session::connect(ProducerConfig::offline()).unwrap();
        let pane = PaneSession::from_session(session).unwrap();
        let debug = format!("{pane:?}");
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("endpoint"));
    }
}
