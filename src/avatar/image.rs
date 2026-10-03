use crate::error::{AppError, ErrorCode};
use image::{
    DynamicImage, GenericImageView, ImageDecoder, ImageFormat, ImageReader, imageops::FilterType,
};
use std::io::Cursor;

pub fn format(content_type: &str) -> Result<(ImageFormat, &'static str), AppError> {
    match content_type {
        "image/jpeg" => Ok((ImageFormat::Jpeg, "jpg")),
        "image/png" => Ok((ImageFormat::Png, "png")),
        "image/webp" => Ok((ImageFormat::WebP, "webp")),
        _ => Err(AppError::validation(
            ErrorCode::UnsupportedAvatarContentType,
            "content_type",
            "unsupported avatar content type",
        )),
    }
}

fn valid_webp_container(bytes: &[u8]) -> bool {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return false;
    }
    let size = u32::from_le_bytes(bytes[4..8].try_into().expect("four bytes")) as usize;
    if size.checked_add(8) != Some(bytes.len()) {
        return false;
    }
    let mut offset = 12usize;
    while offset < bytes.len() {
        let Some(header_end) = offset.checked_add(8) else {
            return false;
        };
        let Some(header) = bytes.get(offset..header_end) else {
            return false;
        };
        let length = u32::from_le_bytes(header[4..8].try_into().expect("four bytes")) as usize;
        let Some(end) = header_end
            .checked_add(length)
            .and_then(|end| end.checked_add(length % 2))
        else {
            return false;
        };
        if end > bytes.len() {
            return false;
        }
        offset = end;
    }
    true
}

pub fn normalize_avatar(bytes: &[u8], declared_type: &str) -> Result<Vec<u8>, AppError> {
    let (format, _) = format(declared_type)?;
    if image::guess_format(bytes).ok() != Some(format) {
        return Err(AppError::validation(
            ErrorCode::UnsupportedAvatarContentType,
            "content_type",
            "unsupported avatar content type",
        ));
    }
    let invalid = || AppError::bad_request(ErrorCode::AvatarInvalidImage, "invalid avatar image");
    if format == ImageFormat::WebP && !valid_webp_container(bytes) {
        return Err(invalid());
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|_| invalid())?;
    if decoder.total_bytes() > 64 * 1024 * 1024 {
        return Err(invalid());
    }
    let orientation = decoder.orientation().map_err(|_| invalid())?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(|_| invalid())?;
    image.apply_orientation(orientation);
    let side = image.width().min(image.height());
    if side == 0 {
        return Err(invalid());
    }
    let x = (image.width() - side) / 2;
    let y = (image.height() - side) / 2;
    macro_rules! resize {
        ($buffer:ident) => {
            image::imageops::resize(
                &*$buffer.view(x, y, side, side),
                512,
                512,
                FilterType::Lanczos3,
            )
        };
    }
    let square = match &image {
        DynamicImage::ImageLuma8(buffer) => DynamicImage::ImageLuma8(resize!(buffer)),
        DynamicImage::ImageLumaA8(buffer) => DynamicImage::ImageLumaA8(resize!(buffer)),
        DynamicImage::ImageRgb8(buffer) => DynamicImage::ImageRgb8(resize!(buffer)),
        DynamicImage::ImageRgba8(buffer) => DynamicImage::ImageRgba8(resize!(buffer)),
        DynamicImage::ImageLuma16(buffer) => DynamicImage::ImageLuma16(resize!(buffer)),
        DynamicImage::ImageLumaA16(buffer) => DynamicImage::ImageLumaA16(resize!(buffer)),
        DynamicImage::ImageRgb16(buffer) => DynamicImage::ImageRgb16(resize!(buffer)),
        DynamicImage::ImageRgba16(buffer) => DynamicImage::ImageRgba16(resize!(buffer)),
        DynamicImage::ImageRgb32F(buffer) => DynamicImage::ImageRgb32F(resize!(buffer)),
        DynamicImage::ImageRgba32F(buffer) => DynamicImage::ImageRgba32F(resize!(buffer)),
        _ => return Err(invalid()),
    }
    .to_rgba8();
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(square)
        .write_to(&mut output, ImageFormat::WebP)
        .map_err(|_| invalid())?;
    Ok(output.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalized_images_are_static_square_webp() {
        for format in [ImageFormat::Jpeg, ImageFormat::Png, ImageFormat::WebP] {
            let mut input = Cursor::new(Vec::new());
            DynamicImage::new_rgb8(96, 32)
                .write_to(&mut input, format)
                .unwrap();
            let content_type = match format {
                ImageFormat::Jpeg => "image/jpeg",
                ImageFormat::Png => "image/png",
                _ => "image/webp",
            };
            let bytes = normalize_avatar(input.get_ref(), content_type).unwrap();
            assert_eq!(image::guess_format(&bytes).unwrap(), ImageFormat::WebP);
            assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 512);
            assert_eq!(image::load_from_memory(&bytes).unwrap().height(), 512);
            assert!(!bytes.windows(4).any(|w| w == b"EXIF" || w == b"ANIM"));
        }
    }
    #[test]
    fn dimensions_and_decoded_allocation_are_bounded() {
        for (width, height) in [(8193, 1), (8192, 3000)] {
            let mut input = Cursor::new(Vec::new());
            DynamicImage::new_rgb8(width, height)
                .write_to(&mut input, ImageFormat::Png)
                .unwrap();
            assert!(normalize_avatar(input.get_ref(), "image/png").is_err());
        }
    }

    #[test]
    fn jpeg_orientation_is_applied_and_not_preserved_as_metadata() {
        let mut image = DynamicImage::new_rgb8(16, 16).to_rgb8();
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x * 12) as u8, (y * 12) as u8, 0]);
        }
        let mut input = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(image)
            .write_to(&mut input, ImageFormat::Jpeg)
            .unwrap();
        let plain = input.into_inner();
        let exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
        let mut oriented = plain[..2].to_vec();
        oriented.extend_from_slice(&[0xff, 0xe1]);
        oriented.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        oriented.extend_from_slice(exif);
        oriented.extend_from_slice(&plain[2..]);
        let actual = normalize_avatar(&oriented, "image/jpeg").unwrap();
        let original = normalize_avatar(&plain, "image/jpeg").unwrap();
        let actual = image::load_from_memory(&actual).unwrap().to_rgb8();
        let expected = image::load_from_memory(&original)
            .unwrap()
            .rotate90()
            .to_rgb8();
        for (x, y) in [(64, 64), (64, 447), (447, 64), (447, 447)] {
            for (actual, expected) in actual
                .get_pixel(x, y)
                .0
                .into_iter()
                .zip(expected.get_pixel(x, y).0)
            {
                assert!(
                    actual.abs_diff(expected) <= 2,
                    "orientation pixel mismatch: {actual} vs {expected}"
                );
            }
        }
    }

    fn small_webp() -> Vec<u8> {
        let mut output = Cursor::new(Vec::new());
        DynamicImage::new_rgb8(2, 2)
            .write_to(&mut output, ImageFormat::WebP)
            .unwrap();
        output.into_inner()
    }

    #[test]
    fn webp_rejects_container_length_beyond_actual_bytes() {
        let mut bytes = small_webp();
        let declared = bytes.len() as u32 + 16;
        bytes[4..8].copy_from_slice(&declared.to_le_bytes());
        assert!(normalize_avatar(&bytes, "image/webp").is_err());
    }

    #[test]
    fn webp_rejects_chunk_without_required_padding() {
        let mut bytes = small_webp();
        bytes.extend_from_slice(b"JUNK\x01\0\0\0x");
        let length = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&length.to_le_bytes());
        assert!(normalize_avatar(&bytes, "image/webp").is_err());
    }

    #[test]
    fn webp_metadata_length_is_checked_without_allocating_it() {
        let mut bytes = small_webp();
        bytes.extend_from_slice(b"EXIF");
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        let length = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&length.to_le_bytes());
        assert!(!valid_webp_container(&bytes));
    }

    #[test]
    fn borrowed_crop_preserves_grayscale_and_sixteen_bit_pixels() {
        let inputs = [
            DynamicImage::ImageLuma8(image::ImageBuffer::from_fn(32, 12, |x, y| {
                image::Luma([(x * 3 + y * 5) as u8])
            })),
            DynamicImage::ImageLumaA16(image::ImageBuffer::from_fn(32, 12, |x, y| {
                image::LumaA([(x * 1000 + y * 500) as u16, 40000])
            })),
            DynamicImage::ImageRgb16(image::ImageBuffer::from_fn(32, 12, |x, y| {
                image::Rgb([(x * 1000) as u16, (y * 3000) as u16, 10000])
            })),
            DynamicImage::ImageRgba16(image::ImageBuffer::from_fn(32, 12, |x, y| {
                image::Rgba([(x * 1000) as u16, (y * 3000) as u16, 10000, 40000])
            })),
        ];
        for input in inputs {
            let mut bytes = Cursor::new(Vec::new());
            input.write_to(&mut bytes, ImageFormat::Png).unwrap();
            let decoded = image::load_from_memory(bytes.get_ref()).unwrap();
            let expected = decoded
                .crop_imm(10, 0, 12, 12)
                .resize_exact(512, 512, FilterType::Lanczos3)
                .to_rgba8();
            let actual = normalize_avatar(bytes.get_ref(), "image/png").unwrap();
            let actual = image::load_from_memory(&actual).unwrap().to_rgba8();
            assert!(actual.pixels().eq(expected.pixels()));
        }
    }

    #[test]
    fn webp_preserves_safe_exif_orientation_and_strips_metadata() {
        let plain = small_webp();
        let exif = b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
        let mut bytes = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\x08\0\0\0\x01\0\0\x01\0\0".to_vec();
        bytes.extend_from_slice(&plain[12..]);
        bytes.extend_from_slice(b"EXIF");
        bytes.extend_from_slice(&(exif.len() as u32).to_le_bytes());
        bytes.extend_from_slice(exif);
        if exif.len() % 2 == 1 {
            bytes.push(0);
        }
        let length = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&length.to_le_bytes());
        assert!(valid_webp_container(&bytes));
        let mut decoder = ImageReader::with_format(Cursor::new(&bytes), ImageFormat::WebP)
            .into_decoder()
            .unwrap();
        assert_eq!(
            decoder.orientation().unwrap(),
            image::metadata::Orientation::Rotate90
        );
        let normalized = normalize_avatar(&bytes, "image/webp").unwrap();
        assert!(!normalized.windows(4).any(|window| window == b"EXIF"));
    }

    #[test]
    fn corrupted_and_mismatched_images_are_rejected() {
        assert!(normalize_avatar(b"not an image", "image/png").is_err());
        assert!(normalize_avatar(b"\x89PNG\r\n\x1a\n", "image/png").is_err());
    }
}
