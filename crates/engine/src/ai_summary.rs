//! A read-only, bounded snapshot of PDF text selected for an AI request.

use std::fmt::Write as _;

use pdfcraft_render::{PageRenderer, RenderRequest, RequestKind, text::PageText};

use crate::Document;

pub const MAX_PAGES: usize = 50;
pub const MAX_TRANSLATION_PAGES: usize = 10;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_TRANSLATION_BYTES: usize = 12 * 1024;
const MAX_ANNOTATIONS_PER_PAGE: usize = 10_000;

#[derive(Clone)]
pub struct SummarySource {
    bytes: std::sync::Arc<Vec<u8>>,
    pub(crate) display: std::sync::Arc<Vec<u8>>,
    pub(crate) config: pdfcraft_render::RenderConfig,
    pub(crate) first: usize,
    pub(crate) last: usize,
    pages: Vec<pdfcraft_render::PageInfo>,
}

impl SummarySource {
    /// Capture the current working document and a 1-based inclusive page range.
    pub fn new(doc: &Document, first: usize, last: usize) -> Result<Self, String> {
        Self::for_range(doc, first, last, MAX_PAGES)
    }

    pub(crate) fn for_range(doc: &Document, first: usize, last: usize, max_pages: usize) -> Result<Self, String> {
        doc.check_copying().map_err(|e| e.to_string())?;
        if first == 0 || last < first || last > doc.info.pages.len() {
            return Err("Select a page range within the document.".into());
        }
        if last - first >= max_pages {
            return Err(format!("Select at most {max_pages} pages at a time."));
        }
        let pages = doc.info.pages.get(first - 1..last).ok_or("Select a page range within the document.")?.to_vec();
        Ok(Self { bytes: doc.bytes.clone(), display: doc.display.clone(), config: doc.config.clone(), first, last, pages })
    }

    pub fn extract(self) -> Result<String, String> {
        self.extract_bounded(MAX_TEXT_BYTES)
    }

    pub fn for_translation(doc: &Document, first: usize, last: usize) -> Result<Self, String> {
        Self::for_range(doc, first, last, MAX_TRANSLATION_PAGES)
    }

    pub fn extract_for_translation(self) -> Result<String, String> {
        self.extract_bounded(MAX_TRANSLATION_BYTES)
    }

    /// Translate a scan or figure's OCR text with source excerpts attached for the reader.
    pub fn translation_with_ocr(
        self,
        options: &crate::ai_ocr::Options,
        region: Option<[f32; 4]>,
        cancelled: impl Fn() -> bool,
    ) -> Result<ScanTranslation, String> {
        let mut reader = crate::ai_ocr::Reader::new(options)?;
        let mut input = String::new();
        let mut passages = Vec::new();
        self.visit_pages_with_ocr(Some(&mut reader), region, &cancelled, |page, text| {
            let excerpt = text.plain_text();
            let excerpt = excerpt.trim();
            if excerpt.is_empty() {
                return Ok(());
            }
            write_page(&mut input, page, excerpt, MAX_TRANSLATION_BYTES)?;
            passages.push((page, excerpt.to_owned(), text.line_rects(0..text.glyphs.len())));
            Ok(())
        })?;
        if passages.is_empty() {
            return Err("Local OCR found no readable text in the selected area.".into());
        }
        Ok(ScanTranslation { input, passages, report: reader.report })
    }

    pub fn figure_png(self, rect: [f32; 4]) -> Result<Vec<u8>, String> {
        crate::guard(|| {
            self.checked_document()?;
            if self.first != self.last {
                return Err("Select a figure on one page.".into());
            }
            let info = self.pages.first().ok_or("Select a figure on an open page.")?;
            let rect = crate::ai_ocr::figure_rect(rect)?;
            let scale = (1600.0 / (rect[2] - rect[0]).max(rect[3] - rect[1])).clamp(0.1, 3.0).min(2800.0 / info.width.max(info.height).max(1.0));
            let settings = crate::ocr::OcrSettings { skip_text_pages: false, region: Some(rect.map(|v| v * scale)), ..Default::default() };
            let config = pdfcraft_render::RenderConfig { system_fonts: false, reading_colors: Default::default(), ..self.config };
            let mut renderer = PageRenderer::new(self.display, config);
            let shot = crate::ocr::prepare_shot(&mut renderer, self.first - 1, info.clone(), &settings, scale)
                .map_err(|result| result.skipped.unwrap_or_else(|| "Could not render the selected figure.".into()))?;
            let image = shot.image().ok_or("Could not render the selected figure.")?;
            crate::export::encode_png(image.width(), image.height(), image.rgba())
        })?
    }

    fn extract_bounded(self, max_bytes: usize) -> Result<String, String> {
        let mut out = String::new();
        self.visit_text(|page, text| write_page(&mut out, page, text, max_bytes))?;
        Ok(out)
    }

    pub(crate) fn visit_text(self, mut visit: impl FnMut(usize, &str) -> Result<(), String>) -> Result<(), String> {
        let mut found = false;
        self.visit_pages(|page, text| {
            let text = text.plain_text();
            if !text.trim().is_empty() {
                found = true;
                visit(page, text.trim())?;
            }
            Ok(())
        })?;
        if !found {
            return Err(NO_SELECTABLE_TEXT.into());
        }
        Ok(())
    }

    fn visit_pages(self, visit: impl FnMut(usize, &PageText) -> Result<(), String>) -> Result<(), String> {
        self.visit_pages_with_ocr(None, None, &|| false, visit)
    }

    pub(crate) fn visit_pages_with_ocr(
        self,
        mut ocr: Option<&mut crate::ai_ocr::Reader>,
        region: Option<[f32; 4]>,
        cancelled: &impl Fn() -> bool,
        mut visit: impl FnMut(usize, &PageText) -> Result<(), String>,
    ) -> Result<(), String> {
        crate::guard(|| {
            let pdf = self.checked_document()?;
            let config = pdfcraft_render::RenderConfig { system_fonts: false, reading_colors: Default::default(), ..self.config };
            let mut renderer = PageRenderer::new(self.display, config);
            for page in self.first - 1..self.last {
                if cancelled() {
                    return Err("Request stopped.".into());
                }
                let result = renderer.render(RenderRequest { page, kind: RequestKind::Text, scale: 1.0, ..Default::default() });
                if result.error.is_some() {
                    return Err(format!("Could not extract text from page {}.", page + 1));
                }
                let text = result.text.ok_or_else(|| format!("Could not extract text from page {}.", page + 1))?;
                if text.truncated {
                    return Err(format!("Page {} has more text than PdfCraft can extract safely.", page + 1));
                }
                let info = self.pages.get(page + 1 - self.first).ok_or("The selected page is unavailable.")?;
                if let Some(ocr) = ocr.as_deref_mut().filter(|_| region.is_some() || needs_ocr(&text, &pdf, page, info)) {
                    let recognized = ocr.read(&mut renderer, page, info, region, Some(&text))?;
                    if cancelled() {
                        return Err("Request stopped.".into());
                    }
                    visit(page + 1, &recognized)?;
                } else {
                    visit(page + 1, &text)?;
                }
            }
            Ok(())
        })?
    }

    fn checked_document(&self) -> Result<pdfcraft_cos::Document, String> {
        use pdfcraft_cos::Object;

        // The display's comment list may omit annotations once it hits its inspection budget.
        let doc = pdfcraft_cos::Document::open_with_password(self.bytes.clone(), self.config.password.as_deref())
            .map_err(|_| "Could not check the PDF for unapplied redactions.")?;
        let pages = pdfcraft_model::pages(&doc);
        let pages = pages.get(self.first - 1..self.last).ok_or("Could not check the selected pages for unapplied redactions.")?;
        for page in pages {
            let Some(annotations) = page.dict.get(b"Annots").map(|a| doc.resolve(a)) else { continue };
            if matches!(annotations.as_ref(), Object::Null) {
                continue;
            }
            let annotations = annotations.as_array().ok_or("Could not read the selected pages' annotations.")?;
            if annotations.len() > MAX_ANNOTATIONS_PER_PAGE {
                return Err("A selected page has too many annotations to check for redactions.".into());
            }
            for annotation in annotations {
                let annotation = doc.resolve(annotation);
                let subtype = annotation.as_dict().and_then(|a| a.get(b"Subtype")).map(|s| doc.resolve(s));
                if subtype.as_deref().and_then(Object::as_name) == Some(b"Redact") {
                    return Err("The selected pages have unapplied redactions. Apply them before using AI on those pages.".into());
                }
            }
        }
        Ok(doc)
    }
}

fn needs_ocr(text: &PageText, doc: &pdfcraft_cos::Document, page: usize, info: &pdfcraft_render::PageInfo) -> bool {
    let glyphs = text.glyphs.iter().filter(|g| !g.text.trim().is_empty()).take(97).count();
    if glyphs == 0 {
        return true;
    }
    // A header or page number layered over a full-page scan does not make its body searchable.
    if glyphs > 96 {
        return false;
    }
    let [x0, y0, x1, y1] = info.crop.map(f64::from);
    let half_page_area = (x1 - x0) * (y1 - y0) * 0.5;
    pdfcraft_edit::page_images(doc, page).is_ok_and(|images| {
        images
            .iter()
            .filter(|image| image.width > 1 && image.height > 1)
            .filter_map(|image| image.visible_rect)
            .any(|[l, b, r, t]| (r.min(x1) - l.max(x0)).max(0.0) * (t.min(y1) - b.max(y0)).max(0.0) >= half_page_area)
    })
}

type ScanPassage = (usize, String, Vec<[f32; 4]>);

pub struct ScanTranslation {
    pub input: String,
    pub passages: Vec<ScanPassage>,
    pub report: crate::ai_ocr::Report,
}

fn write_page(out: &mut String, page: usize, text: &str, max_bytes: usize) -> Result<(), String> {
    if out.len().saturating_add(text.len()).saturating_add(32) > max_bytes {
        return Err("This range has too much text. Select fewer pages.".into());
    }
    let _ = writeln!(out, "[Page {page}]\n{text}\n");
    Ok(())
}

pub(crate) const NO_SELECTABLE_TEXT: &str =
    "The selected pages have no selectable text. For scanned pages, enable local OCR in Ask or Translate, or run Scan & OCR first.";
