//! The file formats that a screenshot can be saved in.

use std::{io::Cursor, path::Path};

use anyhow::{Context, Result};

/// Quality of JPEG files from 1 to 100.
const JPEG_QUALITY: u8 = 90;

/// One file format for screenshots.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ImageFormat {
    /// Lossless and the default.
    #[default]
    Png,
    /// Lossy and small.
    Jpeg,
    /// Lossless WebP.
    Webp,
    /// Uncompressed.
    Bmp,
    /// At most 256 colors.
    Gif,
    /// Lossless, for print tools.
    Tiff,
}

impl ImageFormat {
    /// Every format in menu order.
    pub const ALL: [Self; 6] = [
        Self::Png,
        Self::Jpeg,
        Self::Webp,
        Self::Bmp,
        Self::Gif,
        Self::Tiff,
    ];

    /// Returns the file extension without the dot.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
            Self::Bmp => "bmp",
            Self::Gif => "gif",
            Self::Tiff => "tiff",
        }
    }

    /// Returns the name shown in menus.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Webp => "WebP",
            Self::Bmp => "BMP",
            Self::Gif => "GIF",
            Self::Tiff => "TIFF",
        }
    }

    /// Returns the media type for uploads.
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Bmp => "image/bmp",
            Self::Gif => "image/gif",
            Self::Tiff => "image/tiff",
        }
    }

    /// Reads a format from its name or extension, without regard to case.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_start_matches('.').to_ascii_lowercase();
        match text.as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "webp" => Some(Self::Webp),
            "bmp" => Some(Self::Bmp),
            "gif" => Some(Self::Gif),
            "tif" | "tiff" => Some(Self::Tiff),
            _ => None,
        }
    }

    /// Reads the format from the extension of a path.
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Self> {
        Self::parse(&path.extension()?.to_string_lossy())
    }

    /// Returns the format after this one, for a menu that cycles.
    #[must_use]
    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|format| *format == self);
        Self::ALL[(index.unwrap_or(0) + 1) % Self::ALL.len()]
    }

    /// Encodes packed RGB pixels in this format.
    pub fn encode(self, rgb: Vec<u8>, width: u32, height: u32) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        if self == Self::Jpeg {
            jpeg_into(&mut bytes, &rgb, width, height, JPEG_QUALITY)?;
            return Ok(bytes);
        }
        let target = match self {
            Self::Png | Self::Jpeg => image::ImageFormat::Png,
            Self::Webp => image::ImageFormat::WebP,
            Self::Bmp => image::ImageFormat::Bmp,
            Self::Gif => image::ImageFormat::Gif,
            Self::Tiff => image::ImageFormat::Tiff,
        };
        let picture = image::RgbImage::from_raw(width, height, rgb)
            .context("the image buffer has the wrong size")?;
        image::DynamicImage::ImageRgb8(picture)
            .write_to(&mut Cursor::new(&mut bytes), target)
            .with_context(|| format!("could not encode the {} image", self.name()))?;
        Ok(bytes)
    }
}

/// Writes packed RGB pixels as a JPEG image.
fn jpeg_into(bytes: &mut Vec<u8>, rgb: &[u8], width: u32, height: u32, quality: u8) -> Result<()> {
    use image::ImageEncoder;

    image::codecs::jpeg::JpegEncoder::new_with_quality(bytes, quality)
        .write_image(rgb, width, height, image::ExtendedColorType::Rgb8)
        .context("could not encode the JPEG image")
}

/// Encodes packed RGB pixels as a JPEG image of the given quality.
///
/// The desktop program sends its live preview in this form.
pub fn jpeg(rgb: &[u8], width: u32, height: u32, quality: u8) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    jpeg_into(&mut bytes, rgb, width, height, quality)?;
    Ok(bytes)
}

/// Reports whether a path has the extension of a screenshot format.
#[must_use]
pub fn is_image_file(path: &Path) -> bool {
    ImageFormat::from_path(path).is_some()
}

/// Returns the bytes of a saved screenshot in a form that a web view shows.
///
/// A web view does not decode TIFF, so a TIFF file is converted to PNG.
pub fn viewable_bytes(path: &Path) -> Result<Vec<u8>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    if ImageFormat::from_path(path) != Some(ImageFormat::Tiff) {
        return Ok(bytes);
    }
    let picture = image::load_from_memory_with_format(&bytes, image::ImageFormat::Tiff)
        .with_context(|| format!("could not decode {}", path.display()))?;
    let mut png = Vec::new();
    picture
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .context("could not convert the image")?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small picture with a few flat colors, so every format keeps it.
    fn sample() -> (Vec<u8>, u32, u32) {
        let (width, height) = (16_u32, 8_u32);
        let rgb = (0..width * height)
            .flat_map(|index| {
                if index % width < 8 {
                    [220, 40, 40]
                } else {
                    [20, 60, 200]
                }
            })
            .collect();
        (rgb, width, height)
    }

    #[test]
    fn names_and_extensions_select_a_format() {
        assert_eq!(ImageFormat::parse("JPEG"), Some(ImageFormat::Jpeg));
        assert_eq!(ImageFormat::parse(".tif"), Some(ImageFormat::Tiff));
        assert_eq!(ImageFormat::parse("heic"), None);
        assert_eq!(
            ImageFormat::from_path(Path::new("a/shot.WebP")),
            Some(ImageFormat::Webp)
        );
        assert!(is_image_file(Path::new("shot.jpg")));
        assert!(!is_image_file(Path::new("clip.mp4")));
        assert_eq!(ImageFormat::Tiff.next(), ImageFormat::Png);
        for format in ImageFormat::ALL {
            assert_eq!(ImageFormat::parse(format.extension()), Some(format));
            assert_eq!(ImageFormat::parse(format.name()), Some(format));
        }
    }

    #[test]
    fn every_format_encodes_a_picture_that_decodes_again() {
        let (rgb, width, height) = sample();
        for format in ImageFormat::ALL {
            let bytes = format.encode(rgb.clone(), width, height).unwrap();
            let decoded = image::load_from_memory(&bytes)
                .unwrap_or_else(|error| panic!("{}: {error}", format.name()))
                .to_rgb8();
            assert_eq!((decoded.width(), decoded.height()), (width, height));
            let left = decoded.get_pixel(2, 4).0;
            let right = decoded.get_pixel(13, 4).0;
            assert!(left[0] > 180 && left[2] < 90, "{}: {left:?}", format.name());
            assert!(
                right[2] > 160 && right[0] < 70,
                "{}: {right:?}",
                format.name()
            );
        }
    }

    #[test]
    fn a_tiff_file_becomes_a_png_for_the_web_view() {
        let (rgb, width, height) = sample();
        let folder = tempfile::tempdir().unwrap();
        let tiff = folder.path().join("shot.tiff");
        std::fs::write(
            &tiff,
            ImageFormat::Tiff
                .encode(rgb.clone(), width, height)
                .unwrap(),
        )
        .unwrap();
        let shown = viewable_bytes(&tiff).unwrap();
        assert_eq!(
            image::guess_format(&shown).unwrap(),
            image::ImageFormat::Png
        );
        let jpeg_file = folder.path().join("shot.jpg");
        let encoded = jpeg(&rgb, width, height, 80).unwrap();
        std::fs::write(&jpeg_file, &encoded).unwrap();
        assert_eq!(viewable_bytes(&jpeg_file).unwrap(), encoded);
    }
}
