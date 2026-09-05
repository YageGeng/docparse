use std::fmt::Write as _;
use std::io::Cursor;

use docparse_layout::PageImage;
use image::ImageEncoder;
use image::codecs::png::PngEncoder;
use typed_builder::TypedBuilder;

use crate::{PageResult, RenderError};

/// Encoded raster background and matching inspectable SVG overlay.
#[derive(Debug, Clone, PartialEq, Eq, TypedBuilder)]
pub struct OverlayArtifacts {
    pub png: Vec<u8>,
    pub svg: String,
}

/// Stateless diagnostic renderer for one already parsed and rendered page.
pub struct OverlayRenderer;

impl OverlayRenderer {
    /// Encodes one RGB page and overlays source regions, blocks, and line baselines.
    pub fn render_page(
        image: &PageImage,
        page: &PageResult,
        image_href: &str,
    ) -> Result<OverlayArtifacts, RenderError> {
        let mut png = Cursor::new(Vec::new());
        PngEncoder::new(&mut png).write_image(
            image.data().as_ref(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgb8,
        )?;
        let mut svg = String::new();
        write!(
            svg,
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {} {}\">\n<image href=\"{}\" x=\"0\" y=\"0\" width=\"{}\" height=\"{}\"/>\n",
            page.width,
            page.height,
            xml_escape(image_href),
            page.width,
            page.height
        )?;
        for block in &page.blocks {
            if let Some(source) = &block.source_region {
                writeln!(
                    svg,
                    "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"none\" stroke=\"#ff9f1c\" stroke-dasharray=\"4 3\"/>",
                    source.bbox.left,
                    source.bbox.top,
                    source.bbox.width(),
                    source.bbox.height()
                )?;
            }
            let raw_label = block.raw_label.as_deref().unwrap_or("fallback");
            writeln!(
                svg,
                "<g><title>{} {} #{}</title><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"none\" stroke=\"#00b4d8\"/><text x=\"{}\" y=\"{}\" font-family=\"monospace\" font-size=\"4\" fill=\"#d00000\">#{} {}</text></g>",
                xml_escape(block.id.as_str()),
                xml_escape(raw_label),
                block.final_order,
                block.bbox.left,
                block.bbox.top,
                block.bbox.width(),
                block.bbox.height(),
                block.bbox.left,
                (block.bbox.top + 4.0).max(4.0),
                block.final_order,
                xml_escape(raw_label)
            )?;
            for line in &block.lines {
                if let Some(baseline) = line.baseline {
                    writeln!(
                        svg,
                        "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"#8338ec\" stroke-width=\"0.7\"/>",
                        baseline.start.x,
                        baseline.start.y,
                        baseline.end.x,
                        baseline.end.y
                    )?;
                }
            }
        }
        svg.push_str("</svg>\n");
        Ok(OverlayArtifacts::builder()
            .png(png.into_inner())
            .svg(svg)
            .build())
    }
}

/// Escapes user/model-derived text before embedding it in XML attributes or nodes.
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
