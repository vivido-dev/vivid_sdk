//! Bounded embedded color profiles converted to straight sRGB rows.

// Rust guideline compliant 2026-10-07
use super::container::PngColor;
use moxcms::{
    Chromaticity, CicpProfile, ColorPrimaries, ColorProfile, DataColorSpace, Layout,
    ParsingOptions, ToneReprCurve, TransformOptions, curve_from_gamma,
};

pub(super) fn profile(png: &PngColor, icc: Option<&[u8]>) -> Result<Option<ColorProfile>, ()> {
    // PNG cICP takes precedence over ICC, sRGB and legacy color chunks.
    if let Some([primaries, transfer, matrix, range]) = png.cicp {
        if matrix != 0 || range != 1 {
            return Err(());
        }
        let cicp = CicpProfile {
            color_primaries: primaries.try_into().map_err(|_| ())?,
            transfer_characteristics: transfer.try_into().map_err(|_| ())?,
            matrix_coefficients: matrix.try_into().map_err(|_| ())?,
            full_range: true,
        };
        // Reject unspecified/reserved values rather than falling back to incorrect color.
        let _: ColorPrimaries = cicp.color_primaries.try_into().map_err(|_| ())?;
        let _: ToneReprCurve = cicp.transfer_characteristics.try_into().map_err(|_| ())?;
        return Ok(Some(ColorProfile::new_from_cicp(cicp)));
    }
    if let Some(icc) = icc {
        // Bound parser and transform inputs independently of encoded image dimensions.
        let count = icc.get(128..132).ok_or(())?;
        if u32::from_be_bytes(count.try_into().map_err(|_| ())?) > 128 {
            return Err(());
        }
        let options = ParsingOptions {
            max_profile_size: 4 * 1024 * 1024,
            max_allowed_clut_size: 256 * 1024,
            max_allowed_trc_size: 4096,
        };
        return ColorProfile::new_from_slice_with_options(icc, options)
            .map(Some)
            .map_err(|_| ());
    }
    if png.srgb || (png.gamma.is_none() && png.chromaticities.is_none()) {
        return Ok(None);
    }
    let mut profile = ColorProfile::new_srgb();
    if let Some(values) = png.chromaticities {
        let mut xy = [Chromaticity::new(0., 0.); 4];
        for (point, pair) in xy.iter_mut().zip(values.as_chunks::<2>().0) {
            if pair[1] == 0 || u64::from(pair[0]) + u64::from(pair[1]) > 100_000 {
                return Err(());
            }
            *point = Chromaticity::new(pair[0] as f32 / 100_000., pair[1] as f32 / 100_000.);
        }
        let primaries = ColorPrimaries {
            red: xy[1],
            green: xy[2],
            blue: xy[3],
        };
        let matrix = ColorProfile::colorants_matrix(xy[0].to_xyyb(), primaries);
        if matrix.v.iter().flatten().any(|n| !n.is_finite()) {
            return Err(());
        }
        profile.update_rgb_colorimetry(xy[0].to_xyyb(), primaries);
    }
    if let Some(gamma) = png.gamma {
        let curve = curve_from_gamma(100_000. / gamma as f32);
        profile.cicp = None;
        profile.red_trc = Some(curve.clone());
        profile.green_trc = Some(curve.clone());
        profile.blue_trc = Some(curve);
    }
    Ok(Some(profile))
}

pub(super) fn to_srgb(
    rgba: &mut [u8],
    width: u32,
    grayscale: bool,
    profile: ColorProfile,
) -> Result<(), ()> {
    let layout = match profile.color_space {
        DataColorSpace::Rgb => Layout::Rgba,
        DataColorSpace::Gray if grayscale => Layout::GrayAlpha,
        // The image decoder already converts CMYK JPEG to RGB. Applying a CMYK
        // profile to those RGB bytes would be wrong, so fail this decode profile.
        _ => return Err(()),
    };
    let transform = profile
        .create_transform_8bit(
            layout,
            &ColorProfile::new_srgb(),
            Layout::Rgba,
            TransformOptions::default(),
        )
        .map_err(|_| ())?;
    let row_bytes = usize::try_from(width)
        .map_err(|_| ())?
        .checked_mul(4)
        .ok_or(())?;
    let mut output = vec![0; row_bytes];
    let mut gray = if layout == Layout::GrayAlpha {
        vec![0; row_bytes / 2]
    } else {
        Vec::new()
    };
    for row in rgba.chunks_exact_mut(row_bytes) {
        let input = if layout == Layout::GrayAlpha {
            for (pair, pixel) in gray
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip(row.as_chunks::<4>().0)
            {
                pair.copy_from_slice(&[pixel[0], pixel[3]]);
            }
            &gray
        } else {
            &*row
        };
        transform.transform(input, &mut output).map_err(|_| ())?;
        // Preserve straight alpha even if a color-transform backend changes it.
        for (pixel, original) in output
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(row.as_chunks::<4>().0)
        {
            pixel[3] = original[3];
        }
        row.copy_from_slice(&output);
    }
    Ok(())
}
