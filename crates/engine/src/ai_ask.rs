//! Local passage retrieval and exact text selections from open document snapshots.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};

use pdfcraft_render::text::PageText;

use crate::{
    DocId, Document,
    ai_summary::{NO_SELECTABLE_TEXT, SummarySource},
};

pub const MAX_PAGES: usize = 1000;
const MAX_INDEX_BYTES: usize = 4 * 1024 * 1024;
const PASSAGE_BYTES: usize = 1200;
const MAX_HITS: usize = 6;
pub const MAX_DOCUMENTS: usize = MAX_HITS;
pub const MAX_QUESTION_BYTES: usize = 4096;

pub struct Passage {
    pub doc: DocId,
    pub page: usize,
    pub text: String,
    /// Line rectangles in displayed page points, before any view rotation.
    pub rects: Vec<[f32; 4]>,
}

#[derive(Clone, Copy, Debug)]
pub enum Selection {
    /// Inclusive glyph indices on 1-based pages. `head_x` preserves a same-page drag across table rows.
    Run {
        anchor: (usize, usize),
        head: (usize, usize),
        head_x: Option<f32>,
    },
    Rect {
        page: usize,
        rect: [f32; 4],
    },
}

pub struct Source {
    pub doc: DocId,
    snapshot: SummarySource,
    selection: Option<Selection>,
}

/// A cheap, non-owning revision check for a pending UI request. A layer toggle can change the
/// extracted text without changing the edit generation or displayed PDF bytes.
#[derive(Clone)]
pub struct Stamp {
    pub doc: DocId,
    display: Weak<Vec<u8>>,
    layers: Weak<Vec<(i32, i32, bool)>>,
    generation: u64,
}

impl Stamp {
    pub fn matches(&self, doc: &Document) -> bool {
        doc.id == self.doc
            && doc.edit_generation() == self.generation
            && self.display.ptr_eq(&Arc::downgrade(&doc.display))
            && self.layers.ptr_eq(&Arc::downgrade(&doc.config.layers))
            && doc.allows_copying()
    }
}

pub fn stamp(doc: &Document) -> Stamp {
    Stamp { doc: doc.id, display: Arc::downgrade(&doc.display), layers: Arc::downgrade(&doc.config.layers), generation: doc.edit_generation() }
}

/// The bytes and layer choices used for text geometry, including views of modified documents.
pub fn revision_parts(doc: &Document) -> (&[u8], &[(i32, i32, bool)]) {
    (&doc.display, &doc.config.layers)
}

/// Same-named tabs are distinguishable without sending the user's file paths to the provider.
pub fn document_label(doc: &Document, session: &crate::Session) -> String {
    let name: String = doc.name.chars().filter(|ch| !ch.is_control()).take(128).collect();
    if name.is_empty() || session.docs().iter().any(|other| other.id != doc.id && other.name == doc.name) {
        format!("{name} #{}", doc.id.0)
    } else {
        name
    }
}

impl Source {
    pub fn pages(&self) -> (usize, usize) {
        (self.snapshot.first, self.snapshot.last)
    }

    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    pub fn revision_parts(&self) -> (&[u8], &[(i32, i32, bool)]) {
        (&self.snapshot.display, &self.snapshot.config.layers)
    }
}

struct IndexedPassage {
    passage: Passage,
    terms: HashMap<String, usize>,
    length: usize,
}

pub struct Index {
    passages: Vec<IndexedPassage>,
    counts: HashMap<String, usize>,
    average_length: f64,
    pub ocr_report: crate::ai_ocr::Report,
}

pub fn source(doc: &Document, first: usize, last: usize) -> Result<Source, String> {
    Ok(Source { doc: doc.id, snapshot: SummarySource::for_range(doc, first, last, MAX_PAGES)?, selection: None })
}

pub fn selected_source(doc: &Document, selection: Selection) -> Result<Source, String> {
    let (first, last) = match selection {
        Selection::Run { anchor, head, head_x } if head_x.is_none_or(f32::is_finite) => (anchor.0.min(head.0), anchor.0.max(head.0)),
        Selection::Rect { page, rect: [x0, y0, x1, y1] } if [x0, y0, x1, y1].iter().all(|n| n.is_finite()) && x0 != x1 && y0 != y1 => (page, page),
        _ => return Err("Choose a valid text selection.".into()),
    };
    Ok(Source { doc: doc.id, snapshot: SummarySource::for_range(doc, first, last, MAX_HITS)?, selection: Some(selection) })
}

pub fn validate_scope(sources: &[Source]) -> Result<(), String> {
    if sources.is_empty() || sources.len() > MAX_DOCUMENTS {
        return Err(format!("Select 1 to {MAX_DOCUMENTS} open PDFs."));
    }
    let mut seen = HashSet::new();
    let mut pages = 0usize;
    for source in sources {
        if !seen.insert(source.doc) {
            return Err("Select each PDF only once.".into());
        }
        let (first, last) = source.pages();
        pages = pages.saturating_add(last - first + 1);
    }
    if pages > MAX_PAGES {
        return Err(format!("Select at most {MAX_PAGES} pages across the chosen PDFs."));
    }
    if sources.len() > 1 && sources.iter().any(|source| source.selection.is_some()) {
        return Err("Use a text selection or choose PDFs for this question.".into());
    }
    Ok(())
}

fn terms(text: &str) -> impl Iterator<Item = String> + '_ {
    use unicode_segmentation::UnicodeSegmentation as _;
    text.unicode_words().map(str::to_lowercase).filter(|word| {
        !matches!(
            word.as_str(),
            "the"
                | "a"
                | "an"
                | "is"
                | "of"
                | "to"
                | "in"
                | "it"
                | "and"
                | "for"
                | "what"
                | "about"
                | "this"
                | "that"
                | "how"
                | "does"
                | "do"
                | "are"
                | "was"
                | "were"
        )
    })
}

impl Index {
    pub fn build(sources: Vec<Source>, cancelled: impl Fn() -> bool) -> Result<Self, String> {
        Self::build_with_ocr(sources, None, cancelled)
    }

    pub fn build_with_ocr(sources: Vec<Source>, options: Option<&crate::ai_ocr::Options>, cancelled: impl Fn() -> bool) -> Result<Self, String> {
        validate_scope(&sources)?;
        let mut reader = options.map(crate::ai_ocr::Reader::new).transpose()?;
        let mut index = Self { passages: Vec::new(), counts: HashMap::new(), average_length: 0.0, ocr_report: Default::default() };
        let mut bytes = 0usize;
        for Source { doc, snapshot, selection } in sources {
            if cancelled() {
                return Err("Question stopped.".into());
            }
            let region = match selection {
                Some(Selection::Rect { rect, .. }) => Some(rect),
                _ => None,
            };
            let ocr = reader.as_mut().filter(|_| !matches!(selection, Some(Selection::Run { .. })));
            snapshot.visit_pages_with_ocr(ocr, region, &cancelled, |page, text| {
                if cancelled() {
                    return Err("Question stopped.".into());
                }
                if let Some(selection) = selection {
                    let (part, rects) = selected_page(selection, page, text)?;
                    bytes = bytes.saturating_add(part.len());
                    if bytes > MAX_HITS * PASSAGE_BYTES {
                        return Err("The selection contains too much text. Select a shorter passage.".into());
                    }
                    index.push(Passage { doc, page, text: part, rects }, true);
                } else {
                    let mut start = 0;
                    while start < text.glyphs.len() {
                        let end = passage_end(text, start)?;
                        let part = text.text_of(start..end);
                        bytes = bytes.saturating_add(part.len());
                        if bytes > MAX_INDEX_BYTES {
                            return Err("The search scope contains too much text. Select fewer pages.".into());
                        }
                        index.push(Passage { doc, page, text: part, rects: text.line_rects(start..end) }, false);
                        start = end;
                    }
                }
                Ok(())
            })?;
        }
        if bytes == 0 {
            let message = if reader.as_ref().is_some_and(|reader| reader.report.pages_read > 0) {
                "Local OCR found no readable text in the selected pages or area."
            } else {
                NO_SELECTABLE_TEXT
            };
            return Err(message.into());
        }
        if !index.passages.is_empty() {
            index.average_length /= index.passages.len() as f64;
        }
        if let Some(reader) = reader {
            index.ocr_report = reader.report;
        }
        Ok(index)
    }

    fn push(&mut self, mut passage: Passage, selection: bool) {
        let text = passage.text.trim();
        if text.is_empty() {
            return;
        }
        let mut counts = HashMap::new();
        let mut length = 0;
        for term in terms(text) {
            *counts.entry(term).or_insert(0) += 1;
            length += 1;
        }
        if length == 0 && !selection {
            return;
        }
        for term in counts.keys() {
            *self.counts.entry(term.clone()).or_insert(0) += 1;
        }
        self.average_length += length as f64;
        passage.text = text.into();
        self.passages.push(IndexedPassage { passage, terms: counts, length });
    }

    pub fn selection(&self) -> impl Iterator<Item = &Passage> {
        self.passages.iter().map(|entry| &entry.passage)
    }

    pub fn search(&self, question: &str, previous_question: Option<&str>) -> Vec<&Passage> {
        let current: HashSet<_> = terms(question).collect();
        let previous: HashSet<_> = terms(previous_question.unwrap_or_default()).filter(|word| !current.contains(word)).collect();
        let mut query: Vec<_> = current.iter().map(|word| (word, 1.0)).chain(previous.iter().map(|word| (word, 0.4))).collect();
        query.sort_unstable_by(|a, b| a.0.cmp(b.0));
        let n = self.passages.len() as f64;
        let mut scored: Vec<_> = self
            .passages
            .iter()
            .enumerate()
            .filter_map(|(i, passage)| {
                let score: f64 = query
                    .iter()
                    .filter_map(|(word, weight)| {
                        let frequency = *passage.terms.get(*word)? as f64;
                        let containing = *self.counts.get(*word)? as f64;
                        let idf = (1.0 + (n - containing + 0.5) / (containing + 0.5)).ln();
                        let normalization = 1.2 * (0.25 + 0.75 * passage.length as f64 / self.average_length);
                        Some(weight * idf * frequency * 2.2 / (frequency + normalization))
                    })
                    .sum();
                (score > 0.0).then_some((score, i, &passage.passage))
            })
            .collect();
        scored.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        // Offer the best matching passage from each PDF before taking more from a long PDF.
        let mut docs = HashSet::new();
        let mut first = Vec::new();
        let mut rest = Vec::new();
        for (_, _, passage) in scored {
            if docs.insert(passage.doc) {
                first.push(passage);
            } else if rest.len() < MAX_HITS {
                rest.push(passage);
            }
        }
        first.into_iter().chain(rest).take(MAX_HITS).collect()
    }
}

/// Split at glyph boundaries to keep exact geometry even when the same text occurs twice.
fn passage_end(text: &PageText, start: usize) -> Result<usize, String> {
    let mut bytes = 0usize;
    let mut end = start;
    let mut word_break = None;
    while let Some(glyph) = text.glyphs.get(end) {
        let separates = end != start
            && (text.space_before.get(end).copied().unwrap_or_default() || text.line_of.get(end) != text.line_of.get(end.saturating_sub(1)));
        if separates && bytes >= PASSAGE_BYTES / 2 {
            word_break = Some(end);
        }
        bytes = bytes.saturating_add(glyph.text.len()).saturating_add(usize::from(separates));
        if bytes > PASSAGE_BYTES || end - start >= PASSAGE_BYTES {
            return word_break.or((start != end).then_some(end)).ok_or_else(|| "A text glyph exceeds the AI passage size limit.".into());
        }
        end += 1;
    }
    Ok(end)
}

fn selected_page(selection: Selection, page: usize, text: &PageText) -> Result<(String, Vec<[f32; 4]>), String> {
    let by_geometry = |indices: Vec<usize>| (text.column_text(&indices), text.glyph_rects(&indices));
    match selection {
        Selection::Rect { rect, .. } => Ok(by_geometry(text.glyphs_in(rect))),
        Selection::Run { anchor, head, head_x } => {
            let (first, last) = (anchor.min(head), anchor.max(head));
            for (p, glyph) in [first, last] {
                if p == page && text.glyphs.get(glyph).is_none() {
                    return Err("The selection no longer matches this page. Select the text again.".into());
                }
            }
            if anchor.0 == head.0
                && let Some(indices) = head_x.and_then(|x| text.visual_between(anchor.1, head.1, Some(x)))
            {
                return Ok(by_geometry(indices));
            }
            let start = if page == first.0 { first.1 } else { 0 };
            let end = if page == last.0 { last.1.saturating_add(1) } else { text.glyphs.len() };
            Ok((text.text_of(start..end), text.line_rects(start..end)))
        }
    }
}
