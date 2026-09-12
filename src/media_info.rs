//! Encoded-image inspection for the image track kind.
//!
//! An image track declares its encoding, dimensions, and exact byte length before the bytes are
//! sent, so every producer has to read them out of the container first. This lived in the Python
//! package, which meant a second PNG and JPEG parser maintained by hand; a TypeScript package
//! would have made a third. It belongs here, once, next to the configuration it produces.

use std::io;

use vivid_protocol::track::ImageConfiguration;

use crate::constants::{IMAGE_ENCODING_JPEG, IMAGE_ENCODING_PNG};
use crate::wire::invalid_input;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
/// Signature, IHDR length and type, then width and height.
const PNG_HEADER_LENGTH: usize = 24;

/// Inspect PNG or JPEG header metadata and return the supplied byte length.
///
/// The returned configuration claims exactly `data.len()` bytes, carries no content hash, and does
/// not request a cache lookup. Callers that retain images across presentations set
/// [`ImageConfiguration::sha256`] and [`ImageConfiguration::cache_lookup`] themselves.
///
/// This is not a decoder or a completeness check: it reads dimensions from the header, but does
/// not validate image data, checksums, or end markers. Decoding belongs to the presenter.
pub fn probe_encoded_image(data: &[u8]) -> io::Result<ImageConfiguration> {
    let encoded_length = u32::try_from(data.len())
        .map_err(|_| invalid_input("encoded image is larger than a single record body allows"))?;
    if encoded_length == 0 {
        return Err(invalid_input("encoded image is empty"));
    }

    let (encoding, width, height) = if data.starts_with(&PNG_SIGNATURE) {
        probe_png(data)?
    } else if data.starts_with(&[0xff, 0xd8]) {
        probe_jpeg(data)?
    } else {
        return Err(invalid_input(
            "only PNG and JPEG image headers are supported",
        ));
    };

    let configuration = ImageConfiguration {
        encoding,
        width,
        height,
        encoded_length,
        sha256: None,
        cache_lookup: false,
    };
    // Dimensions come from the container and are not otherwise bounded, so let the protocol's own
    // validation reject an image this session could never carry.
    configuration
        .validate()
        .map_err(|error| invalid_input(error.to_string()))?;
    Ok(configuration)
}

fn probe_png(data: &[u8]) -> io::Result<(u64, u32, u32)> {
    if data.len() < PNG_HEADER_LENGTH {
        return Err(invalid_input("PNG image is truncated before its header"));
    }
    if &data[12..16] != b"IHDR" {
        return Err(invalid_input("PNG image does not begin with an IHDR chunk"));
    }
    let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
    Ok((IMAGE_ENCODING_PNG, width, height))
}

fn probe_jpeg(data: &[u8]) -> io::Result<(u64, u32, u32)> {
    // Walk the marker stream to the first start-of-frame segment. Every segment carries its own
    // length, so the walk is bounded by the input and cannot spin on a malformed file.
    let mut offset = 2_usize;
    while offset.saturating_add(4) <= data.len() {
        if data[offset] != 0xff {
            return Err(invalid_input("JPEG image has an invalid marker stream"));
        }
        let marker = data[offset + 1];
        offset += 2;

        // Padding and the standalone start and end markers carry no length.
        if marker == 0xff || marker == 0xd8 || marker == 0xd9 {
            continue;
        }

        let length = usize::from(u16::from_be_bytes([data[offset], data[offset + 1]]));
        if length < 2 || offset.saturating_add(length) > data.len() {
            return Err(invalid_input("JPEG image has a truncated segment"));
        }

        // SOF0 through SOF3 are the baseline and progressive frame headers that declare size.
        // SOF4 (0xc4) is a Huffman table, not a frame header.
        if (0xc0..=0xc3).contains(&marker) {
            if length < 7 {
                return Err(invalid_input("JPEG image has an invalid frame header"));
            }
            let height = u32::from(u16::from_be_bytes([data[offset + 3], data[offset + 4]]));
            let width = u32::from(u16::from_be_bytes([data[offset + 5], data[offset + 6]]));
            return Ok((IMAGE_ENCODING_JPEG, width, height));
        }
        offset += length;
    }
    Err(invalid_input("JPEG image ends before a frame header"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut data = PNG_SIGNATURE.to_vec();
        data.extend_from_slice(&13_u32.to_be_bytes());
        data.extend_from_slice(b"IHDR");
        data.extend_from_slice(&width.to_be_bytes());
        data.extend_from_slice(&height.to_be_bytes());
        data.extend_from_slice(&[8, 6, 0, 0, 0]);
        data
    }

    fn jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut data = vec![0xff, 0xd8];
        // An APP0 segment the walk must skip before it reaches the frame header.
        data.extend_from_slice(&[0xff, 0xe0, 0x00, 0x06, b'J', b'F', b'I', b'F']);
        data.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
        data.extend_from_slice(&height.to_be_bytes());
        data.extend_from_slice(&width.to_be_bytes());
        data.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        data
    }

    #[test]
    fn reads_png_dimensions() {
        let data = png(640, 480);
        let configuration = probe_encoded_image(&data).expect("PNG");
        assert_eq!(configuration.encoding, IMAGE_ENCODING_PNG);
        assert_eq!((configuration.width, configuration.height), (640, 480));
        assert_eq!(configuration.encoded_length as usize, data.len());
        assert_eq!(configuration.sha256, None);
        assert!(!configuration.cache_lookup);
    }

    /// JPEG stores height before width, which is the inversion this parser exists to get right.
    #[test]
    fn reads_jpeg_dimensions_in_wire_order() {
        let configuration = probe_encoded_image(&jpeg(1024, 768)).expect("JPEG");
        assert_eq!(configuration.encoding, IMAGE_ENCODING_JPEG);
        assert_eq!((configuration.width, configuration.height), (1024, 768));
    }

    #[test]
    fn rejects_unknown_and_empty_containers() {
        assert!(probe_encoded_image(&[]).is_err());
        assert!(probe_encoded_image(b"GIF89a").is_err());
    }

    /// A truncated container must fail here rather than become a track whose declared length
    /// never arrives.
    #[test]
    fn rejects_truncated_images() {
        let data = png(640, 480);
        assert!(probe_encoded_image(&data[..20]).is_err());
        let data = jpeg(64, 64);
        assert!(probe_encoded_image(&data[..6]).is_err());
    }

    #[test]
    fn rejects_png_without_an_ihdr_chunk() {
        let mut data = png(8, 8);
        data[12..16].copy_from_slice(b"tEXt");
        assert!(probe_encoded_image(&data).is_err());
    }

    /// A dimension the protocol refuses is refused here, not at track creation.
    #[test]
    fn rejects_dimensions_the_protocol_does_not_admit() {
        assert!(probe_encoded_image(&png(0, 480)).is_err());
        assert!(probe_encoded_image(&png(9000, 480)).is_err());
    }

    /// 0xc4 is a Huffman table that sits inside the same numeric neighbourhood as the frame
    /// headers; treating it as one would report a table's bytes as an image size.
    #[test]
    fn does_not_mistake_a_huffman_table_for_a_frame_header() {
        let mut data = vec![0xff, 0xd8];
        data.extend_from_slice(&[0xff, 0xc4, 0x00, 0x09, 0, 1, 2, 3, 4, 5, 6]);
        data.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
        data.extend_from_slice(&32_u16.to_be_bytes());
        data.extend_from_slice(&16_u16.to_be_bytes());
        data.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        let configuration = probe_encoded_image(&data).expect("JPEG");
        assert_eq!((configuration.width, configuration.height), (16, 32));
    }
}
