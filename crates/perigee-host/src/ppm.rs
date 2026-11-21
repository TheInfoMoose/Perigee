//! Binary PPM (P6) encoding for captured and received frames, used by `perigee-host snapshot`
//! and `perigee connect --dump-frame` as proof that real pixels flow.

use anyhow::{Result, bail};
use perigee_protocol::PixelFormat;

/// Encodes a 4-byte-per-pixel `Bgra8` or `Rgba8` image as a binary PPM (P6, 8-bit RGB, alpha
/// dropped). `stride` is the byte length of one source row (at least `width * 4`).
pub fn encode_ppm(
    width: u32,
    height: u32,
    stride: u32,
    format: PixelFormat,
    data: &[u8],
) -> Result<Vec<u8>> {
    let (r, g, b) = match format {
        PixelFormat::Bgra8 => (2usize, 1usize, 0usize),
        PixelFormat::Rgba8 => (0, 1, 2),
        other => bail!("PPM export supports Bgra8 and Rgba8 frames, not {other:?}"),
    };
    if width == 0 || height == 0 {
        bail!("cannot write an empty {width}x{height} frame");
    }
    let row_bytes = width as usize * 4;
    let stride = stride as usize;
    if stride < row_bytes {
        bail!("stride {stride} is shorter than one {width}-pixel row");
    }
    let needed = stride * (height as usize - 1) + row_bytes;
    if data.len() < needed {
        bail!(
            "frame data is {} bytes; {width}x{height} at stride {stride} needs {needed}",
            data.len()
        );
    }
    let header = format!("P6\n{width} {height}\n255\n");
    let mut out = Vec::with_capacity(header.len() + width as usize * height as usize * 3);
    out.extend_from_slice(header.as_bytes());
    for row in 0..height as usize {
        let line = &data[row * stride..row * stride + row_bytes];
        for px in line.as_chunks::<4>().0 {
            out.extend_from_slice(&[px[r], px[g], px[b]]);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_ppm_bgra_with_padded_stride() {
        // 2x1 image, stride 12 (4 padding bytes), pixels in B, G, R, A order.
        let data = [10, 20, 30, 255, 40, 50, 60, 255, 0, 0, 0, 0];
        let ppm = encode_ppm(2, 1, 12, PixelFormat::Bgra8, &data).unwrap();
        assert_eq!(&ppm[..11], b"P6\n2 1\n255\n");
        assert_eq!(&ppm[11..], &[30, 20, 10, 60, 50, 40]);
    }

    #[test]
    fn test_encode_ppm_rgba_and_errors() {
        let data = [1, 2, 3, 4];
        assert_eq!(
            &encode_ppm(1, 1, 4, PixelFormat::Rgba8, &data).unwrap()[11..],
            &[1, 2, 3]
        );
        assert!(encode_ppm(1, 1, 4, PixelFormat::NV12, &data).is_err());
        assert!(encode_ppm(2, 1, 4, PixelFormat::Rgba8, &data).is_err());
        assert!(encode_ppm(1, 2, 4, PixelFormat::Rgba8, &data).is_err());
        assert!(encode_ppm(0, 1, 4, PixelFormat::Rgba8, &data).is_err());
    }
}
