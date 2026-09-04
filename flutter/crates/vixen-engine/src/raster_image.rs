use std::io::Cursor;
use std::sync::Arc;

use crate::doc::{DocumentImage, DocumentImageItem};
use crate::page::Page;

pub(crate) const MAX_RASTER_IMAGE_BODY_BYTES: u64 = 8 * 1024 * 1024;
pub(crate) const MAX_RASTER_IMAGE_DIMENSION: u32 = 2048;
pub(crate) const MAX_DECODED_RASTER_IMAGE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RasterImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
}

pub(crate) struct PageImageRunner {
    items: std::vec::IntoIter<DocumentImageItem>,
    csp: vixen_net::csp::ContentSecurityPolicy,
    origin: vixen_net::Origin,
    context_trustworthy: bool,
}

pub(crate) enum PreparedPageImage {
    Skip,
    External(ExternalPageImage),
}

#[derive(Clone)]
pub(crate) struct ExternalPageImage {
    node_id: usize,
    url: url::Url,
    csp: vixen_net::csp::ContentSecurityPolicy,
    origin: vixen_net::Origin,
    context_trustworthy: bool,
}

impl PageImageRunner {
    pub(crate) fn new(page: &Page) -> Self {
        let document_url = url::Url::parse(page.url()).ok();
        Self {
            items: page.document().image_execution_items().into_iter(),
            csp: page.csp().clone(),
            origin: document_url
                .as_ref()
                .map(vixen_net::Origin::from_url)
                .unwrap_or_else(vixen_net::Origin::opaque),
            context_trustworthy: document_url
                .as_ref()
                .is_some_and(vixen_net::referrer_policy::is_potentially_trustworthy),
        }
    }

    pub(crate) fn prepare_next(&mut self, page: &Page) -> Option<PreparedPageImage> {
        let item = self.items.next()?;
        match item {
            DocumentImageItem::CspMeta(policy) => {
                self.csp.add_header(&policy);
                Some(PreparedPageImage::Skip)
            }
            DocumentImageItem::Image(DocumentImage { node_id, src }) => {
                let Some(url) = page
                    .resolve_url(&src)
                    .and_then(|resolved| url::Url::parse(&resolved).ok())
                else {
                    return Some(PreparedPageImage::Skip);
                };
                let request = ExternalPageImage {
                    node_id,
                    url,
                    csp: self.csp.clone(),
                    origin: self.origin.clone(),
                    context_trustworthy: self.context_trustworthy,
                };
                Some(if request.allows_url(request.url()) {
                    PreparedPageImage::External(request)
                } else {
                    PreparedPageImage::Skip
                })
            }
        }
    }
}

impl ExternalPageImage {
    pub(crate) fn node_id(&self) -> usize {
        self.node_id
    }

    pub(crate) fn url(&self) -> &url::Url {
        &self.url
    }

    pub(crate) fn allows_url(&self, url: &url::Url) -> bool {
        self.blocked_reason(url).is_none()
    }

    pub(crate) fn blocked_reason(&self, url: &url::Url) -> Option<&'static str> {
        if !self.csp.allows_fetch("img-src", url, &self.origin) {
            return Some("csp");
        }
        if !matches!(
            vixen_net::classify_mixed_content(
                self.context_trustworthy,
                url,
                vixen_net::ResourceType::Image,
                false,
            ),
            vixen_net::MixedContentVerdict::NotMixed
        ) {
            return Some("mixed-content");
        }
        None
    }

    pub(crate) fn is_cross_site(&self, url: &url::Url) -> bool {
        !vixen_net::is_same_site(&self.origin, &vixen_net::Origin::from_url(url))
    }
}

pub(crate) fn decode_png(bytes: &[u8]) -> Result<RasterImage, String> {
    if bytes.len() as u64 > MAX_RASTER_IMAGE_BODY_BYTES {
        return Err("PNG body exceeds the raster-image limit".to_owned());
    }

    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_ignore_text_chunk(true);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    decoder.set_limits(png::Limits {
        bytes: MAX_DECODED_RASTER_IMAGE_BYTES,
    });
    let header = decoder
        .read_header_info()
        .map_err(|error| format!("invalid PNG header: {error}"))?;
    validate_dimensions(header.width, header.height)?;

    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("invalid PNG metadata: {error}"))?;
    if reader.info().animation_control.is_some() {
        return Err("animated PNG is outside the bounded raster-image vertical".to_owned());
    }
    let output_len = reader
        .output_buffer_size()
        .ok_or_else(|| "PNG output size overflow".to_owned())?;
    if output_len > MAX_DECODED_RASTER_IMAGE_BYTES {
        return Err("PNG decoded output exceeds the raster-image limit".to_owned());
    }
    let mut decoded = vec![0; output_len];
    let output = reader
        .next_frame(&mut decoded)
        .map_err(|error| format!("PNG decode failed: {error}"))?;
    validate_dimensions(output.width, output.height)?;
    if output.bit_depth != png::BitDepth::Eight {
        return Err("PNG decoder did not normalize to 8-bit channels".to_owned());
    }
    decoded.truncate(output.buffer_size());
    let rgba = normalize_rgba(decoded, output.color_type, output.width, output.height)?;
    Ok(RasterImage {
        width: output.width,
        height: output.height,
        rgba: Arc::new(rgba),
    })
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), String> {
    let decoded_bytes = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4));
    if width == 0
        || height == 0
        || width > MAX_RASTER_IMAGE_DIMENSION
        || height > MAX_RASTER_IMAGE_DIMENSION
        || decoded_bytes.is_none_or(|bytes| bytes > MAX_DECODED_RASTER_IMAGE_BYTES)
    {
        return Err(format!(
            "PNG dimensions {width}x{height} exceed the raster-image limit"
        ));
    }
    Ok(())
}

fn normalize_rgba(
    decoded: Vec<u8>,
    color_type: png::ColorType,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let pixels = width as usize * height as usize;
    let channels = match color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => {
            return Err("PNG palette was not expanded by the decoder".to_owned());
        }
    };
    if decoded.len() != pixels * channels {
        return Err("PNG decoder returned an inconsistent pixel buffer".to_owned());
    }
    if channels == 4 {
        return Ok(decoded);
    }
    let mut rgba = Vec::with_capacity(pixels * 4);
    for pixel in decoded.chunks_exact(channels) {
        match color_type {
            png::ColorType::Grayscale => {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], 255])
            }
            png::ColorType::GrayscaleAlpha => {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]])
            }
            png::ColorType::Rgb => rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]),
            png::ColorType::Rgba | png::ColorType::Indexed => unreachable!(),
        }
    }
    Ok(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_rgba(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(rgba).unwrap();
        writer.finish().unwrap();
        bytes
    }

    #[test]
    fn decodes_rgba_png_without_changing_pixels() {
        let expected = [255, 0, 0, 255, 0, 128, 255, 64];
        let image = decode_png(&encode_rgba(2, 1, &expected)).unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.rgba.as_slice(), expected);
    }

    #[test]
    fn rejects_oversized_dimensions_before_output_allocation() {
        let bytes = encode_rgba(MAX_RASTER_IMAGE_DIMENSION + 1, 1, &vec![0; 2049 * 4]);
        let error = decode_png(&bytes).unwrap_err();
        assert!(error.contains("dimensions"));
    }

    #[test]
    fn image_src_and_mixed_content_are_checked() {
        let page = Page::from_html(
            "https://document.test/page",
            "<img src='https://images.test/pixel.png'>",
        )
        .unwrap();
        let mut runner = PageImageRunner::new(&page);
        let Some(PreparedPageImage::External(request)) = runner.prepare_next(&page) else {
            panic!("allowed image was not prepared");
        };
        assert_eq!(request.node_id(), 4);
        assert!(request.allows_url(request.url()));
        assert_eq!(
            request.blocked_reason(&url::Url::parse("http://images.test/pixel.png").unwrap()),
            Some("mixed-content")
        );
    }
}
