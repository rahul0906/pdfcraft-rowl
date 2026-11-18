//! Read scan text for AI without writing a text layer into the document.

use std::collections::HashSet;

use pdfcraft_render::{
    PageInfo, PageRenderer,
    text::{PageText, TextGlyph, layout},
};

use crate::ocr::{self, EngineChoice, OcrSettings};
pub use pdfcraft_ocr::tesseract::LANGUAGE_NAMES as LANGUAGES;

pub const MAX_SCAN_PAGES: usize = 20;
const MAX_WORDS: usize = 12_000;

pub(crate) fn figure_rect([x0, y0, x1, y1]: [f32; 4]) -> Result<[f32; 4], String> {
    if ![x0, y0, x1, y1].iter().all(|v| v.is_finite()) || x0 == x1 || y0 == y1 {
        return Err("Draw a rectangle over the figure.".into());
    }
    Ok([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)])
}

#[derive(Clone, Debug)]
pub struct Options {
    pub language: String,
    pub engine: EngineChoice,
}

impl Default for Options {
    fn default() -> Self {
        Self { language: "en".into(), engine: EngineChoice::Auto }
    }
}

#[derive(Default, Clone, Debug)]
pub struct Report {
    pub pages_read: usize,
    pub low_confidence_words: usize,
}

impl Report {
    pub fn notice(&self) -> Option<String> {
        (self.low_confidence_words > 0)
            .then(|| format!("Local OCR reported {} low-confidence words. Verify them on the source page.", self.low_confidence_words))
    }
}

pub(crate) struct Reader {
    settings: OcrSettings,
    engines: Option<ocr::Recognizers>,
    pub report: Report,
}

impl Reader {
    pub fn new(options: &Options) -> Result<Self, String> {
        if !LANGUAGES.iter().any(|(code, _)| *code == options.language) {
            return Err("Choose a supported source language for OCR.".into());
        }
        // The lightweight ocrs model reads English. Other languages need the matching
        // Tesseract language pack even when the engine is set to Automatic.
        let engine = match (options.language.as_str(), options.engine) {
            ("en", engine) => engine,
            (_, EngineChoice::Ocrs) => return Err("ocrs reads English. Choose Tesseract or Automatic for this source language.".into()),
            (_, _) => EngineChoice::Tesseract,
        };
        Ok(Self {
            settings: OcrSettings {
                language: options.language.clone(),
                engine,
                strategy: engine.default_strategy(),
                skip_text_pages: false,
                ..OcrSettings::default()
            },
            engines: None,
            report: Report::default(),
        })
    }

    pub fn read(
        &mut self,
        renderer: &mut PageRenderer,
        page: usize,
        info: &PageInfo,
        region: Option<[f32; 4]>,
        native: Option<&PageText>,
    ) -> Result<PageText, String> {
        if self.report.pages_read >= MAX_SCAN_PAGES {
            return Err(format!("Select at most {MAX_SCAN_PAGES} scanned pages at a time."));
        }
        let region = region.map(figure_rect).transpose()?;
        let engines = match &self.engines {
            Some(engines) => engines,
            None => self.engines.insert(ocr::recognizers(&self.settings).map_err(|e| format!("Local OCR is unavailable: {e}"))?),
        };
        self.report.pages_read += 1;
        let scale = (300.0 / 72.0_f32).min(3600.0 / info.width.max(info.height).max(1.0));
        self.settings.region = region.map(|r| r.map(|v| v * scale));
        let shot = ocr::prepare_shot(renderer, page, info.clone(), &self.settings, scale)
            .map_err(|page| page.skipped.unwrap_or_else(|| "Could not render the scan.".into()))?;
        let result = shot.read(engines);
        if let Some(error) = result.skipped {
            return Err(error);
        }
        if result.words.len() > MAX_WORDS {
            return Err("The scanned page has too many words. Select a smaller area.".into());
        }
        self.report.low_confidence_words += result.suspects().len();
        let mut glyphs = Vec::new();
        if let Some(native) = native {
            match region {
                Some(rect) => glyphs.extend(native.glyphs_in(rect).into_iter().filter_map(|i| native.glyphs.get(i).cloned())),
                None => glyphs.extend(native.glyphs.iter().cloned()),
            }
        }
        let mut ocr_boxes = HashSet::new();
        glyphs.extend(result.words.into_iter().filter_map(|word| {
            let [x, y] = word.origin;
            let [dx, dy] = word.across;
            let [ux, uy] = word.up;
            let quad = [[x, y], [x + dx, y + dy], [x + dx + ux, y + dy + uy], [x + ux, y + uy]].map(|[x, y]| info.user_to_view(x as f32, y as f32));
            let [a, b, c, d] = quad;
            let rect = [
                a[0].min(b[0]).min(c[0]).min(d[0]),
                a[1].min(b[1]).min(c[1]).min(d[1]),
                a[0].max(b[0]).max(c[0]).max(d[0]),
                a[1].max(b[1]).max(c[1]).max(d[1]),
            ];
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let length = dx.hypot(dy);
            if !length.is_finite() || length == 0.0 || !rect.iter().all(|v| v.is_finite()) {
                return None;
            }
            ocr_boxes.insert(rect.map(f32::to_bits));
            Some(TextGlyph { text: word.text, rect, quad, direction: [dx / length, dy / length] })
        }));
        let mut text = layout(glyphs);
        // The recognizer emits words, unlike the renderer's character glyphs. Geometry
        // determines reading order, but equally spaced words still need word boundaries.
        if !matches!(self.settings.language.as_str(), "zh" | "ja") {
            for (index, pair) in text.glyphs.windows(2).enumerate() {
                let [previous, current] = pair else { continue };
                if (ocr_boxes.contains(&previous.rect.map(f32::to_bits)) || ocr_boxes.contains(&current.rect.map(f32::to_bits)))
                    && !previous.text.ends_with(char::is_whitespace)
                    && !current.text.starts_with(char::is_whitespace)
                    && let Some(space) = text.space_before.get_mut(index + 1)
                {
                    *space = true;
                }
            }
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pdfcraft_ocr::{Line, OcrError, OcrImage, RecognizeOptions, Recognizer, Word};

    use super::*;
    use crate::{Session, ai_summary::SummarySource};

    struct Columns;

    impl Recognizer for Columns {
        fn id(&self) -> &'static str {
            "columns"
        }

        fn available(&self) -> bool {
            true
        }

        fn languages(&self) -> Vec<(String, String)> {
            vec![("en".into(), "English".into())]
        }

        fn recognize(&self, image: &OcrImage, options: &RecognizeOptions) -> Result<Vec<Line>, OcrError> {
            assert_eq!(options.language, "en");
            assert!(image.width() < 1600, "the recognizer should receive only the crop");
            let line = |x, y, labels: [&str; 2]| Line {
                words: vec![
                    Word::new(labels[0], [x, y, x + 125.0, y + 60.0], "columns").with_confidence(90.0),
                    Word::new(labels[1], [x + 131.0, y, x + 230.0, y + 60.0], "columns").with_confidence(28.0),
                ],
            };
            Ok(vec![
                line(40.0, 60.0, ["Copper", "one"]),
                line(950.0, 60.0, ["Silver", "one"]),
                line(40.0, 150.0, ["Copper", "two"]),
                line(950.0, 150.0, ["Silver", "two"]),
                line(40.0, 240.0, ["Copper", "three"]),
                line(950.0, 240.0, ["Silver", "three"]),
            ])
        }
    }

    #[test]
    fn ai_ocr_keeps_words_columns_boxes_and_confidence() {
        let mut session = Session::new();
        let bytes = session.create_blank(400.0, 300.0, 1).unwrap();
        let id = session.open_new("columns.pdf", bytes.clone()).unwrap();
        let info = &session.get(id).unwrap().info.pages[0];
        let mut reader = Reader::new(&Options::default()).unwrap();
        reader.engines = Some(ocr::Recognizers { primary: Arc::new(Columns), secondary: None });
        let mut renderer = PageRenderer::new(bytes, Default::default());
        let text = reader.read(&mut renderer, 0, info, Some([24.0, 24.0, 376.0, 280.0]), None).unwrap();
        assert_eq!(text.plain_text(), "Copper one\nCopper two\nCopper three\nSilver one\nSilver two\nSilver three");
        assert!(text.glyphs.iter().all(|glyph| glyph.rect[0] >= 24.0 && glyph.rect[1] >= 24.0));
        assert!(text.glyphs[6].rect[0] > 200.0);
        assert_eq!(reader.report.pages_read, 1);
        assert_eq!(reader.report.low_confidence_words, 6);
        assert!(reader.report.notice().unwrap().contains("6 low-confidence"));
    }

    #[test]
    fn ai_ocr_cancellation_and_unavailable_engine_have_actionable_errors() {
        let mut session = Session::new();
        let bytes = session.create_blank(200.0, 300.0, 1).unwrap();
        let id = session.open_new("scan.pdf", bytes.clone()).unwrap();
        let doc = session.get(id).unwrap();
        let options = Options { language: "fr".into(), engine: EngineChoice::Auto };
        let source = SummarySource::for_translation(doc, 1, 1).unwrap();
        let error = source.translation_with_ocr(&options, None, || true).err().unwrap();
        assert!(error.contains("stopped"));

        let mut reader = Reader::new(&options).unwrap();
        assert_eq!(reader.settings.engine, EngineChoice::Tesseract);
        reader.settings.tesseract_path = Some("not/an/executable".into());
        let mut renderer = PageRenderer::new(bytes, Default::default());
        let error = reader.read(&mut renderer, 0, &doc.info.pages[0], None, None).unwrap_err();
        assert!(error.contains("Local OCR is unavailable") && error.contains("tesseract"), "{error}");
        assert_eq!(reader.report.pages_read, 0);
    }
}
