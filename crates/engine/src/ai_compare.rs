//! Bounded, cited text differences for explaining PDF revisions.

use std::fmt::Write as _;

use pdfcraft_compare::{Kind, Side, Word};

use crate::{Document, ai_summary::SummarySource};

const MAX_PAGES: usize = 250;
const MAX_WORDS: usize = 100_000;
const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_CHANGES: usize = 48;
const MAX_PROMPT_BYTES: usize = 16 * 1024;
const CONTEXT_WORDS: usize = 8;

pub struct Snapshot {
    old: SummarySource,
    new: SummarySource,
    pages: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Replaced,
    Inserted,
    Deleted,
    Moved,
}

impl ChangeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Replaced => "Replaced",
            Self::Inserted => "Inserted",
            Self::Deleted => "Deleted",
            Self::Moved => "Moved",
        }
    }
}

pub struct Passage {
    /// One-based page, with rectangles in displayed page points.
    pub page: usize,
    pub text: String,
    pub rects: Vec<[f32; 4]>,
}

pub struct Excerpt {
    pub text: String,
    pub before: String,
    pub after: String,
    pub passages: Vec<Passage>,
}

pub struct Change {
    pub id: usize,
    pub kind: ChangeKind,
    pub old: Option<Excerpt>,
    pub new: Option<Excerpt>,
}

impl Change {
    /// Render the facts locally. Provider output can only change their order.
    pub fn description(&self) -> String {
        let old = self.old.as_ref().map_or("", |side| side.text.as_str());
        let new = self.new.as_ref().map_or("", |side| side.text.as_str());
        let period = |text: &str| if text.ends_with(['.', '!', '?']) { "" } else { "." };
        match self.kind {
            ChangeKind::Replaced => format!("Changed “{old}” to “{new}”{}", period(new)),
            ChangeKind::Inserted => format!("Added “{new}”{}", period(new)),
            ChangeKind::Deleted => format!("Removed “{old}”{}", period(old)),
            ChangeKind::Moved => format!("Moved “{old}”{}", period(old)),
        }
    }
}

pub struct Evidence {
    pub changes: Vec<Change>,
    pub prompt: String,
}

impl Evidence {
    /// Keep each verified change exactly once, even if the provider omits or invents IDs.
    pub fn ranked(self, order: &[usize]) -> Vec<Change> {
        let mut pending: Vec<_> = self.changes.into_iter().map(Some).collect();
        let mut ranked = Vec::with_capacity(pending.len());
        for &id in order {
            if let Some(change) = id.checked_sub(1).and_then(|i| pending.get_mut(i)).and_then(Option::take) {
                ranked.push(change);
            }
        }
        ranked.extend(pending.into_iter().flatten());
        ranked
    }
}

impl Snapshot {
    /// `pages` refers to the newer document (zero-based, empty for every page).
    pub fn new(old: &Document, new: &Document, pages: &[usize]) -> Result<Self, String> {
        let mut pages = pages.to_vec();
        pages.sort_unstable();
        pages.dedup();
        if pages.last().is_some_and(|&p| p >= new.info.pages.len()) {
            return Err("Select pages within the newer PDF.".into());
        }
        let capture = |doc: &Document| SummarySource::for_range(doc, 1, doc.info.pages.len(), MAX_PAGES);
        Ok(Self { old: capture(old)?, new: capture(new)?, pages })
    }

    pub fn extract(self, stopped: impl Fn() -> bool) -> Result<Evidence, String> {
        crate::guard(|| {
            let old = words(self.old, &stopped)?;
            let new = words(self.new, &stopped)?;
            if old.is_empty() || new.is_empty() {
                return Err("Both PDFs need selectable text for this comparison. Use Scan & OCR for image-only PDFs.".into());
            }
            let comparison = pdfcraft_compare::compare(&old, &new);
            if stopped() {
                return Err("Request stopped.".into());
            }
            let mut selected = Vec::new();
            for (i, change) in comparison.changes.iter().enumerate() {
                let range = change.new.page..=change.new.last_page;
                if !self.pages.is_empty() {
                    if !self.pages.iter().any(|p| range.contains(p)) {
                        continue;
                    }
                    if !range.clone().all(|p| self.pages.binary_search(&p).is_ok()) {
                        return Err("A text change crosses the selected page range. Include all pages of the change.".into());
                    }
                }
                if selected.len() == MAX_CHANGES {
                    return Err(format!("There are more than {MAX_CHANGES} text changes. Select fewer pages in the newer PDF."));
                }
                selected.push(i);
            }
            let changes = &comparison.changes;
            let excerpt = |i: usize, get_side: fn(&pdfcraft_compare::Change) -> &Side, all: &[Word]| {
                let Some(side) = changes.get(i).map(get_side).filter(|side| !side.word_range.is_empty()) else { return Ok(None) };
                let bounds = (
                    i.checked_sub(1).and_then(|p| changes.get(p)).map_or(0, |prev| get_side(prev).word_range.end),
                    changes.get(i + 1).map_or(all.len(), |next| get_side(next).word_range.start),
                );
                Excerpt::new(side, all, bounds).map(Some)
            };
            let mut evidence = Evidence { changes: Vec::new(), prompt: String::new() };
            for &i in &selected {
                let Some(change) = changes.get(i) else { continue };
                let (kind, new_index) = match change.kind {
                    Kind::Deleted => match move_partner(changes, &selected, i, Kind::Inserted) {
                        Some(dest) => (ChangeKind::Moved, dest),
                        None => (ChangeKind::Deleted, i),
                    },
                    Kind::Inserted if move_partner(changes, &selected, i, Kind::Deleted).is_some() => continue,
                    Kind::Inserted => (ChangeKind::Inserted, i),
                    Kind::Replaced => (ChangeKind::Replaced, i),
                };
                let change = Change {
                    id: evidence.changes.len() + 1,
                    kind,
                    old: excerpt(i, |ch| &ch.old, &old)?,
                    new: excerpt(new_index, |ch| &ch.new, &new)?,
                };
                let _ = writeln!(evidence.prompt, "ID {} {}", change.id, kind.label());
                for (version, side) in [("Older", &change.old), ("Newer", &change.new)] {
                    if let Some(side) = side {
                        let _ = writeln!(
                            evidence.prompt,
                            "{version} context {:?}, changed text {:?}, following context {:?}",
                            side.before, side.text, side.after
                        );
                    }
                }
                if evidence.prompt.len() > MAX_PROMPT_BYTES {
                    return Err("The text changes are too large to explain at once. Select fewer pages.".into());
                }
                evidence.changes.push(change);
            }
            Ok(evidence)
        })?
    }
}

fn words(source: SummarySource, stopped: &impl Fn() -> bool) -> Result<Vec<Word>, String> {
    let mut words = Vec::new();
    let mut bytes = 0usize;
    source.visit_pages_with_ocr(None, None, stopped, |page, text| {
        for (text, rect) in crate::js::words_with(text, |x, y| [x, y]) {
            bytes = bytes.saturating_add(text.len());
            if bytes > MAX_TEXT_BYTES || words.len() == MAX_WORDS {
                return Err("A PDF has too much text for an AI comparison.".into());
            }
            words.push(Word { text, page: page - 1, rect: rect.map(f64::from) });
        }
        Ok(())
    })?;
    Ok(words)
}

fn moved_side(change: &pdfcraft_compare::Change) -> Option<&Side> {
    match change.kind {
        Kind::Deleted => Some(&change.old),
        Kind::Inserted => Some(&change.new),
        Kind::Replaced => None,
    }
}

fn move_partner(changes: &[pdfcraft_compare::Change], selected: &[usize], i: usize, opposite: Kind) -> Option<usize> {
    let change = changes.get(i)?;
    let original = moved_side(change)?;
    // Repeated phrases, and individual words shuffled during replacement, are ambiguous.
    if original.word_range.len() < 3 {
        return None;
    }
    let mut same = 0;
    let mut partner = None;
    for &other in selected {
        let Some(candidate) = changes.get(other) else { continue };
        if !moved_side(candidate).is_some_and(|candidate| candidate.text == original.text) {
            continue;
        }
        if candidate.kind == opposite {
            if partner.replace(other).is_some() {
                return None;
            }
        } else {
            same += 1;
        }
    }
    partner.filter(|_| same == 1)
}

impl Excerpt {
    fn new(side: &Side, words: &[Word], (prev, next): (usize, usize)) -> Result<Self, String> {
        if side.text.len() > MAX_PROMPT_BYTES {
            return Err("A single text change is too large to explain. Use the comparison report to see the full change.".into());
        }
        let start = side.word_range.start;
        let end = side.word_range.end;
        let before = words.get(start.saturating_sub(CONTEXT_WORDS).max(prev)..start).unwrap_or_default();
        let after = words.get(end..end.saturating_add(CONTEXT_WORDS).min(next)).unwrap_or_default();
        let context =
            |words: &[Word], page| words.iter().filter(|word| word.page == page).map(|word| word.text.as_str()).collect::<Vec<_>>().join(" ");
        let passages = words
            .get(side.word_range.clone())
            .unwrap_or_default()
            .chunk_by(|a, b| a.page == b.page)
            .filter_map(|part| {
                let page = part.first()?.page + 1;
                Some(Passage {
                    page,
                    text: part.iter().map(|word| word.text.as_str()).collect::<Vec<_>>().join(" "),
                    rects: part.iter().take(1200).map(|word| word.rect.map(|v| v as f32)).collect(),
                })
            })
            .collect();
        Ok(Self { text: side.text.clone(), before: context(before, side.page), after: context(after, side.last_page), passages })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;

    #[test]
    fn ai_compare_pairs_a_moved_passage_with_its_destination() {
        const PASSAGE: &str = "Original protocol maintained.";
        const MAIN: &str = "Recruitment took place over autumn. Outcomes were measured with three different questionnaires.";
        let mut session = Session::new();
        let older = session.create_from_text("Old", &format!("{PASSAGE}\u{c}{MAIN}")).unwrap();
        let newer = session.create_from_text("New", &format!("{MAIN}\u{c}{PASSAGE}")).unwrap();
        let old = session.open_new("paper-v1.pdf", older).unwrap();
        let new = session.open_new("paper-v2.pdf", newer).unwrap();
        let compare = |pages| Snapshot::new(session.get(old).unwrap(), session.get(new).unwrap(), pages).unwrap().extract(|| false).unwrap();
        let evidence = compare(&[]);
        assert_eq!(evidence.changes.len(), 1);
        let change = &evidence.changes[0];
        assert_eq!(change.kind, ChangeKind::Moved);
        assert_eq!(change.description(), format!("Moved “{PASSAGE}”"));
        let old = &change.old.as_ref().unwrap().passages[0];
        let new = &change.new.as_ref().unwrap().passages[0];
        assert_eq!((old.page, new.page), (1, 2));
        assert_eq!(old.text, new.text);
        assert!(!old.rects.is_empty() && !new.rects.is_empty());

        // A narrower comparison includes the inserted end of the move only. The
        // deletion's anchor is on page one of the newer document.
        let selected = compare(&[1]);
        assert_eq!(selected.changes.len(), 1);
        assert_eq!(selected.changes[0].kind, ChangeKind::Inserted);
    }

    #[test]
    fn ai_compare_keeps_page_boxes_separate_for_cross_page_changes() {
        let mut session = Session::new();
        let older = session.create_from_text("Old", "Top earlier\u{c}before bottom").unwrap();
        let newer = session.create_from_text("New", "Top later\u{c}after bottom").unwrap();
        let old = session.open_new("old.pdf", older).unwrap();
        let new = session.open_new("new.pdf", newer).unwrap();
        let old = session.get(old).unwrap();
        let new = session.get(new).unwrap();
        let evidence = Snapshot::new(old, new, &[]).unwrap().extract(|| false).unwrap();
        assert_eq!(evidence.changes.len(), 1);
        let newer = &evidence.changes[0].new.as_ref().unwrap().passages;
        assert_eq!(newer.iter().map(|p| (p.page, p.text.as_str())).collect::<Vec<_>>(), [(1, "later"), (2, "after")]);
        assert!(Snapshot::new(old, new, &[0]).unwrap().extract(|| false).err().unwrap().contains("crosses the selected page range"));
    }
}
