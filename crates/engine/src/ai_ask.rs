//! Local passage retrieval for questions about one document revision.

use std::collections::{HashMap, HashSet};

use crate::{Document, ai_summary::SummarySource};

pub const MAX_PAGES: usize = 1000;
const MAX_INDEX_BYTES: usize = 4 * 1024 * 1024;
const PASSAGE_BYTES: usize = 1200;
const MAX_HITS: usize = 6;
pub const MAX_QUESTION_BYTES: usize = 4096;

pub struct Passage {
    pub page: usize,
    pub text: String,
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
}

pub fn source(doc: &Document, first: usize, last: usize) -> Result<SummarySource, String> {
    SummarySource::for_range(doc, first, last, MAX_PAGES)
}

fn terms(text: &str) -> impl Iterator<Item = String> + '_ {
    use unicode_segmentation::UnicodeSegmentation as _;
    text.unicode_words().map(str::to_lowercase).filter(|word| {
        !matches!(word.as_str(), "the" | "a" | "an" | "is" | "of" | "to" | "in" | "it" | "and" | "for" | "what" | "about" | "this" | "that")
    })
}

impl Index {
    pub fn build(source: SummarySource, cancelled: impl Fn() -> bool) -> Result<Self, String> {
        let mut index = Self { passages: Vec::new(), counts: HashMap::new(), average_length: 0.0 };
        let mut bytes = 0usize;
        source.visit_text(|page, mut text| {
            if cancelled() {
                return Err("Question stopped.".into());
            }
            bytes = bytes.saturating_add(text.len());
            if bytes > MAX_INDEX_BYTES {
                return Err("The search scope contains too much text. Select fewer pages.".into());
            }
            while !text.is_empty() {
                let limit = text.floor_char_boundary(PASSAGE_BYTES.min(text.len()));
                let end = if limit < text.len() {
                    text.get(..limit).and_then(|s| s.rfind(char::is_whitespace)).filter(|&i| i >= limit / 2).unwrap_or(limit)
                } else {
                    limit
                };
                let Some((part, rest)) = text.split_at_checked(end) else { return Err("Could not split document text.".into()) };
                text = rest.trim_start();
                let mut counts = HashMap::new();
                let mut length = 0;
                for term in terms(part) {
                    *counts.entry(term).or_insert(0) += 1;
                    length += 1;
                }
                if length == 0 {
                    continue;
                }
                for term in counts.keys() {
                    *index.counts.entry(term.clone()).or_insert(0) += 1;
                }
                index.average_length += length as f64;
                index.passages.push(IndexedPassage { passage: Passage { page, text: part.trim().into() }, terms: counts, length });
            }
            Ok(())
        })?;
        if !index.passages.is_empty() {
            index.average_length /= index.passages.len() as f64;
        }
        Ok(index)
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
        scored.into_iter().take(MAX_HITS).map(|(_, _, passage)| passage).collect()
    }
}
