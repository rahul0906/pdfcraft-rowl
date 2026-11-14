//! A read-only, bounded snapshot of PDF text selected for summarization.

use std::fmt::Write as _;

use pdfcraft_render::{PageRenderer, RenderRequest, RequestKind};

use crate::Document;

pub const MAX_PAGES: usize = 50;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_ANNOTATIONS_PER_PAGE: usize = 10_000;

pub struct SummarySource {
    bytes: std::sync::Arc<Vec<u8>>,
    display: std::sync::Arc<Vec<u8>>,
    config: pdfcraft_render::RenderConfig,
    first: usize,
    last: usize,
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
        Ok(Self { bytes: doc.bytes.clone(), display: doc.display.clone(), config: doc.config.clone(), first, last })
    }

    pub fn extract(self) -> Result<String, String> {
        let mut out = String::new();
        self.visit_text(|page, text| {
            if out.len().saturating_add(text.len()).saturating_add(32) > MAX_TEXT_BYTES {
                return Err("This range has too much text. Select fewer pages.".into());
            }
            let _ = writeln!(out, "[Page {page}]\n{text}\n");
            Ok(())
        })?;
        Ok(out)
    }

    pub(crate) fn visit_text(self, mut visit: impl FnMut(usize, &str) -> Result<(), String>) -> Result<(), String> {
        crate::guard(|| {
            self.check_redactions()?;
            let mut renderer = PageRenderer::new(self.display, self.config);
            let mut found = false;
            for page in self.first - 1..self.last {
                let result = renderer.render(RenderRequest { page, kind: RequestKind::Text, scale: 1.0, ..Default::default() });
                if result.error.is_some() {
                    return Err(format!("Could not extract text from page {}.", page + 1));
                }
                let text = result.text.ok_or_else(|| format!("Could not extract text from page {}.", page + 1))?;
                if text.truncated {
                    return Err(format!("Page {} has more text than PdfCraft can extract safely.", page + 1));
                }
                let text = text.plain_text();
                let text = text.trim();
                if !text.is_empty() {
                    found = true;
                    visit(page + 1, text)?;
                }
            }
            if !found {
                return Err("The selected pages have no selectable text. Run Scan & OCR on scanned pages first.".into());
            }
            Ok(())
        })?
    }

    fn check_redactions(&self) -> Result<(), String> {
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
        Ok(())
    }
}
