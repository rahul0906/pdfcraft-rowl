//! Multi-turn document chat in the right sidebar.

use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use base64::Engine as _;
use pdfcraft_engine::{
    DocId, Document, Session, ai_ask, ai_ocr,
    ai_summary::{self, SummarySource},
    ocr::EngineChoice,
};
use pdfcraft_platform::{
    ai,
    ai_chat::{self, Citation, CitationLocation, History, SourceInfo, Task, Turn},
};

use crate::ai_ui::{Job, poll, start};
use crate::{PdfCraftApp, RightPanel, theme};

pub(crate) type Chats = HashMap<DocId, Chat>;

type CachedIndex = (String, Arc<ai_ask::Index>);
type Answer = (Option<CachedIndex>, Turn);

const TRANSLATION_LANGUAGES: &[&str] = &[
    "Arabic",
    "Bengali",
    "Chinese (Simplified)",
    "English",
    "French",
    "German",
    "Hindi",
    "Japanese",
    "Marathi",
    "Portuguese",
    "Spanish",
    "Tamil",
    "Telugu",
];

pub(crate) struct Chat {
    generation: u64,
    revision: String,
    path: Option<String>,
    history: History,
    archive: Option<String>,
    confirm_new: bool,
    turns: Vec<Turn>,
    question: String,
    task: Task,
    ask_pages: (usize, usize),
    ask_documents: Option<Vec<(DocId, (usize, usize))>>,
    selection: Option<(ai_ask::Selection, ai_ask::Stamp)>,
    figure: bool,
    vision: bool,
    scan_text: bool,
    ocr_language: &'static str,
    ocr_engine: EngineChoice,
    summary_pages: (usize, usize),
    translation_pages: (usize, usize),
    language: Option<&'static str>,
    pending: Option<String>,
    index: Option<CachedIndex>,
    job: Option<Job<Answer>>,
    job_sources: Vec<ai_ask::Stamp>,
    stop: Arc<AtomicBool>,
    error: Option<String>,
    destination: Result<ai::Profile, String>,
}

impl Drop for Chat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Chat {
    fn new(doc: &Document, current_page: usize) -> Self {
        let revision = ai_chat::revision_id(&doc.bytes);
        let (history, error) = match doc.path.as_deref().map(ai_chat::load).transpose() {
            Ok(history) => (history.unwrap_or_default(), None),
            Err(error) => (History::default(), Some(error)),
        };
        let turns = history
            .revisions
            .iter()
            .find(|entry| entry.id == revision)
            .map(|entry| entry.turns.iter().rev().take(ai_chat::MAX_TURNS).rev().cloned().collect())
            .unwrap_or_default();
        Self {
            generation: doc.edit_generation(),
            revision,
            path: doc.path.clone(),
            history,
            archive: None,
            confirm_new: false,
            turns,
            question: String::new(),
            task: Task::Ask,
            ask_pages: (1, doc.info.pages.len()),
            ask_documents: None,
            selection: None,
            figure: false,
            vision: false,
            scan_text: false,
            ocr_language: "en",
            ocr_engine: EngineChoice::Auto,
            summary_pages: (1, doc.info.pages.len().min(ai_summary::MAX_PAGES)),
            translation_pages: (current_page, current_page),
            language: None,
            pending: None,
            index: None,
            job: None,
            job_sources: Vec::new(),
            stop: Arc::default(),
            error,
            destination: provider(),
        }
    }

    fn persist(&mut self, enabled: bool, update: Option<(&str, &[Turn])>) -> bool {
        let Some(path) = self.path.as_deref() else { return false };
        match ai_chat::save(path, enabled, update) {
            Ok(history) => {
                self.history = history;
                self.error = None;
                true
            }
            Err(error) => {
                self.error = Some(error);
                false
            }
        }
    }

    fn save_current(&mut self) {
        if self.history.enabled {
            let id = self.revision.clone();
            let turns = self.turns.clone();
            self.persist(true, Some((&id, &turns)));
        }
    }

    fn pages_mut(&mut self) -> &mut (usize, usize) {
        match self.task {
            Task::Ask => &mut self.ask_pages,
            Task::Summarize => &mut self.summary_pages,
            Task::Translate => &mut self.translation_pages,
        }
    }

    fn poll_answer(&mut self) {
        if let Some(result) = poll(&mut self.job) {
            self.pending = None;
            self.job_sources.clear();
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            match result {
                Ok((index, turn)) => {
                    if let Some(index) = index {
                        self.index = Some(index);
                    }
                    if turn.task == Task::Ask {
                        self.question.clear();
                    }
                    if self.turns.len() == ai_chat::MAX_TURNS {
                        self.turns.remove(0);
                    }
                    self.turns.push(turn);
                    self.save_current();
                }
                Err(error) => self.error = Some(error),
            }
        }
    }

    fn matches(&self, doc: &Document) -> bool {
        doc.edit_generation() == self.generation && doc.path == self.path
    }

    fn ocr_options(&self) -> Option<ai_ocr::Options> {
        (self.scan_text || self.figure).then(|| ai_ocr::Options { language: self.ocr_language.into(), engine: self.ocr_engine })
    }

    fn figure_selection(&self) -> Option<(usize, [f32; 4])> {
        match self.selection {
            Some((ai_ask::Selection::Rect { page, rect }, _)) if self.figure => Some((page, rect)),
            _ => None,
        }
    }

    fn clear_selection(&mut self) {
        self.selection = None;
        self.figure = false;
        self.vision = false;
    }

    fn send(&mut self, doc: &Document, session: &Session, ctx: &egui::Context) -> Result<(), String> {
        let latest = provider()?;
        let shown = self.destination.as_ref().map_err(Clone::clone)?;
        if latest.endpoint()? != shown.endpoint()? || latest.model != shown.model || latest.name != shown.name {
            self.destination = Ok(latest);
            return Err("The active provider changed. Review the new destination and send again.".into());
        }
        if self.task == Task::Ask {
            return self.send_ask(doc, session, latest, ctx);
        }
        let task = self.task;
        let region = (task == Task::Translate).then(|| self.figure_selection()).flatten();
        let scope = region.map(|(page, _)| (page, page)).unwrap_or_else(|| *self.pages_mut());
        if region.is_some() && !self.selection.as_ref().is_some_and(|(_, stamp)| stamp.matches(doc)) {
            return Err("The selected area changed. Select it again.".into());
        }
        let source = match task {
            Task::Ask => return Err("Choose an AI action.".into()),
            Task::Summarize => SummarySource::new(doc, scope.0, scope.1)?,
            Task::Translate => SummarySource::for_translation(doc, scope.0, scope.1)?,
        };
        let scan = (task == Task::Translate).then(|| self.ocr_options()).flatten().map(|options| {
            let info = SourceInfo::new(doc.id.0, ai_ask::document_label(doc, session), doc.path.as_deref(), ai_ask::revision_parts(doc));
            (options, info)
        });
        let language = self.language;
        let page_range = format!("{}–{}", scope.0, scope.1);
        let question = match task {
            Task::Ask => return Err("Choose an AI action.".into()),
            Task::Summarize => crate::i18n::fmt(tl!("Summarize pages {pages}"), &[("pages", &page_range)]),
            Task::Translate if region.is_some() => {
                let target = language.ok_or("Choose a target language.")?;
                crate::i18n::fmt(tl!("Translate the selected area to {language}"), &[("language", tl!(target))])
            }
            Task::Translate => {
                let target = language.ok_or("Choose a target language.")?;
                crate::i18n::fmt(tl!("Translate pages {pages} to {language}"), &[("pages", &page_range), ("language", tl!(target))])
            }
        };
        self.stop = Arc::default();
        let stop = self.stop.clone();
        let prompt = question.clone();
        let job_sources = vec![ai_ask::stamp(doc)];
        self.job = Some(start(ctx, move || {
            let check_stop = || {
                if stop.load(Ordering::Relaxed) { Err("Request stopped.".to_owned()) } else { Ok(()) }
            };
            check_stop()?;
            let mut citations = Vec::new();
            let mut notice = None;
            let answer = if task == Task::Summarize {
                let text = source.extract()?;
                check_stop()?;
                ai::summarize(&latest, &text)?
            } else if let Some((options, info)) = scan {
                let scan = source.translation_with_ocr(&options, region.map(|(_, rect)| rect), || stop.load(Ordering::Relaxed))?;
                notice = scan.report.notice();
                citations = scan
                    .passages
                    .into_iter()
                    .map(|(page, excerpt, rects)| Citation { page, excerpt, location: Some(CitationLocation { source: info.clone(), rects }) })
                    .collect();
                check_stop()?;
                ai::translate(&latest, language.ok_or("Choose a target language.")?, &scan.input)?
            } else {
                let text = source.extract_for_translation()?;
                check_stop()?;
                ai::translate(&latest, language.ok_or("Choose a target language.")?, &text)?
            };
            Ok((None, Turn { task, question, answer, citations, scope, ask_scope: None, notice }))
        })?);
        self.job_sources = job_sources;
        self.pending = Some(prompt);
        Ok(())
    }

    fn send_ask(&mut self, doc: &Document, session: &Session, profile: ai::Profile, ctx: &egui::Context) -> Result<(), String> {
        let question = self.question.trim();
        if question.is_empty() || question.len() > ai_ask::MAX_QUESTION_BYTES {
            return Err("Enter a question of 1–4096 bytes.".into());
        }
        let selected = self.selection.is_some();
        let current_scope = (!selected && self.ask_documents.is_none()).then_some(self.ask_pages);
        let input = if let Some((selection, stamp)) = &self.selection {
            if !stamp.matches(doc) {
                return Err("The text selection changed. Select the text again or choose PDFs.".into());
            }
            vec![(doc, ai_ask::selected_source(doc, *selection)?)]
        } else if let Some(documents) = &self.ask_documents {
            documents
                .iter()
                .map(|&(id, (first, last))| {
                    let doc = session.get(id).ok_or("A selected PDF was closed. Choose the PDFs again.")?;
                    Ok((doc, ai_ask::source(doc, first, last)?))
                })
                .collect::<Result<_, String>>()?
        } else {
            vec![(doc, ai_ask::source(doc, self.ask_pages.0, self.ask_pages.1)?)]
        };
        let job_sources = input.iter().map(|(doc, _)| ai_ask::stamp(doc)).collect();
        let (documents, sources): (Vec<_>, Vec<_>) =
            input.into_iter().map(|(doc, source)| ((ai_ask::document_label(doc, session), doc.path.clone()), source)).unzip();
        ai_ask::validate_scope(&sources)?;
        let ocr = self.ocr_options();
        let vision = if let Some((page, rect)) = self.figure_selection().filter(|_| self.vision) {
            if !profile.is_llama_cpp() {
                return Err("Choose a local llama.cpp vision profile to include the image.".into());
            }
            Some((page, rect, SummarySource::new(doc, page, page)?))
        } else {
            None
        };
        let cached = self.index.clone();
        let history = if selected { Vec::new() } else { self.turns.clone() };
        self.stop = Arc::default();
        let stop = self.stop.clone();
        let question = question.to_owned();
        let prompt = question.clone();
        self.job = Some(start(ctx, move || {
            let evidence: Vec<_> = sources
                .iter()
                .zip(documents)
                .map(|(source, (name, path))| {
                    (SourceInfo::new(source.doc.0, name, path.as_deref(), source.revision_parts()), source.pages(), source.selection())
                })
                .collect();
            if let Some((page, rect, snapshot)) = vision {
                let options = ocr.as_ref().ok_or("Choose the source language for OCR.")?;
                let (source, _, _) = evidence.first().ok_or("Select an area on a PDF page.")?;
                let png = snapshot.clone().figure_png(rect)?;
                let (labels, notice) = match snapshot.translation_with_ocr(options, Some(rect), || stop.load(Ordering::Relaxed)) {
                    Ok(scan) => (scan.input, scan.report.notice()),
                    Err(error) => (String::new(), Some(error)),
                };
                if stop.load(Ordering::Relaxed) {
                    return Err("Request stopped.".into());
                }
                let answer = ai::ask_figure(&profile, &question, &labels, &base64::engine::general_purpose::STANDARD.encode(png))?;
                let citation = Citation { page, excerpt: labels, location: Some(CitationLocation { source: source.clone(), rects: vec![rect] }) };
                return Ok((None, Turn { task: Task::Ask, question, answer, citations: vec![citation], scope: (0, 0), ask_scope: None, notice }));
            }
            let scope_key = if ocr.is_some() {
                ai_chat::revision_id(format!("{evidence:?}{ocr:?}").as_bytes())
            } else {
                ai_chat::revision_id(format!("{evidence:?}").as_bytes())
            };
            let mut turns: Vec<_> = history
                .into_iter()
                .rev()
                .filter(|turn| {
                    turn.task == Task::Ask
                        && (turn.ask_scope.as_ref() == Some(&scope_key)
                            || (ocr.is_none() && turn.ask_scope.is_none() && current_scope == Some(turn.scope)))
                })
                .take(4)
                .collect();
            turns.reverse();
            let index = match cached {
                Some((key, index)) if key == scope_key => index,
                _ => Arc::new(ai_ask::Index::build_with_ocr(sources, ocr.as_ref(), || stop.load(Ordering::Relaxed))?),
            };
            let hits: Vec<_> =
                if selected { index.selection().collect() } else { index.search(&question, turns.last().map(|turn| turn.question.as_str())) };
            let citations = hits
                .into_iter()
                .filter_map(|passage| {
                    let (source, _, _) = evidence.iter().find(|(source, _, _)| source.doc == passage.doc.0)?;
                    Some(Citation {
                        page: passage.page,
                        excerpt: passage.text.clone(),
                        location: Some(CitationLocation { source: source.clone(), rects: passage.rects.clone() }),
                    })
                })
                .collect();
            if stop.load(Ordering::Relaxed) {
                return Err("Request stopped.".into());
            }
            let mut turn = ai_chat::ask(&profile, &question, &turns, citations)?;
            turn.notice = index.ocr_report.notice();
            turn.scope = current_scope.unwrap_or_default();
            turn.ask_scope = Some(scope_key.clone());
            Ok((Some((scope_key, index)), turn))
        })?);
        self.job_sources = job_sources;
        self.pending = Some(prompt);
        Ok(())
    }
}

fn provider() -> Result<ai::Profile, String> {
    ai::load().and_then(|providers| providers.enabled_profile(None).cloned())
}

pub(crate) fn open(app: &mut PdfCraftApp, task: Task) {
    let Some((view, id)) = app.active_ids() else { return };
    let Some(doc) = app.session.get(id) else { return };
    let current_page = app.views.get(view).map_or(1, |view| view.current.saturating_add(1));
    let chat = chat_for(&mut app.ai_chats, doc, current_page);
    chat.task = task;
    if chat.figure {
        chat.clear_selection();
    }
    chat.destination = provider();
    app.ai_chat_anchor = None;
    app.choose_right_panel(Some(RightPanel::AiChat));
}

pub(crate) fn open_selection(app: &mut PdfCraftApp, view: usize, selection: ai_ask::Selection) {
    open_scope(app, view, selection, Task::Ask, false);
}

pub(crate) fn open_region(app: &mut PdfCraftApp, view: usize, page: usize, rect: [f32; 4], task: Task) {
    open_scope(app, view, ai_ask::Selection::Rect { page, rect }, task, true);
}

fn open_scope(app: &mut PdfCraftApp, view: usize, selection: ai_ask::Selection, task: Task, figure: bool) {
    let Some(view_id) = app.views.get(view).map(|view| view.id) else { return };
    let Some(doc) = app.session.get(view_id) else { return };
    let chat = chat_for(&mut app.ai_chats, doc, app.views.get(view).map_or(1, |view| view.current.saturating_add(1)));
    if chat.job.is_some() {
        chat.error = Some("Stop the current request before asking about a selection.".into());
    } else {
        chat.task = task;
        chat.figure = figure;
        chat.vision = false;
        chat.destination = provider();
        chat.selection = Some((selection, ai_ask::stamp(doc)));
        chat.archive = None;
        chat.error = None;
    }
    app.active = Some(view);
    app.ai_chat_anchor = None;
    app.choose_right_panel(Some(RightPanel::AiChat));
}

pub(crate) fn reveal_citation(app: &mut PdfCraftApp, owner: DocId, page: usize, location: CitationLocation) {
    let target = app.views.iter().enumerate().find_map(|(index, view)| {
        if view.id.0 != location.source.doc {
            return None;
        }
        let doc = app.session.get(view.id)?;
        let page = page.checked_sub(1)?;
        let info = doc.info.pages.get(page)?;
        if !doc.allows_copying() || !location.source.matches(doc.path.as_deref(), ai_ask::revision_parts(doc)) {
            return None;
        }
        let rects: Vec<_> = location
            .rects
            .iter()
            .take(1200)
            .filter_map(|&[x0, y0, x1, y1]| {
                if ![x0, y0, x1, y1].iter().all(|v| v.is_finite()) {
                    return None;
                }
                let [a, b] = info.view_to_user(x0, y0);
                let [c, d] = info.view_to_user(x1, y1);
                Some([a.min(c), b.min(d), a.max(c), b.max(d)])
            })
            .collect();
        Some((index, page, rects, &doc.info))
    });
    let Some((index, page, rects, info)) = target else {
        app.notify_tr("The cited PDF is no longer open at that revision.");
        return;
    };
    let Some(view) = app.views.get_mut(index) else { return };
    if let Some(rect) = rects.first().copied() {
        view.reveal_rect(page, rect, info);
        view.citation_flash = Some((page, rects, 0.0));
    } else {
        view.go_to_page(page);
    }
    app.active = Some(index);
    app.ai_chat_anchor = Some(owner);
}

fn chat_for<'a>(chats: &'a mut Chats, doc: &Document, current_page: usize) -> &'a mut Chat {
    let chat = chats.entry(doc.id).or_insert_with(|| Chat::new(doc, current_page));
    if !chat.matches(doc) {
        *chat = Chat::new(doc, current_page);
    }
    chat
}

pub(crate) fn refresh(chats: &mut Chats, session: &pdfcraft_engine::Session) {
    chats.retain(|id, chat| session.get(*id).is_some_and(|doc| chat.matches(doc) && doc.allows_copying()));
    for chat in chats.values_mut() {
        if chat.job_sources.iter().any(|stamp| !session.get(stamp.doc).is_some_and(|doc| stamp.matches(doc))) {
            chat.stop.store(true, Ordering::Relaxed);
            chat.error = Some("A source PDF changed or closed. Send the question again after reviewing the sources.".into());
        }
        chat.poll_answer();
    }
}

pub(crate) fn panel(
    ui: &mut egui::Ui,
    t: &theme::Tokens,
    doc: &Document,
    session: &Session,
    current_page: usize,
    chats: &mut Chats,
    nav: &mut Option<crate::panels::Nav>,
) -> bool {
    if let Err(error) = doc.check_copying() {
        ui.label(error.to_string());
        return false;
    }
    let chat = chat_for(chats, doc, current_page);
    ui.push_id(doc.id.0, |ui| {
        header(ui, chat, t);
        let settings = if chat.archive.is_none() {
            egui::Panel::bottom("ai-composer")
                .resizable(false)
                .show_separator_line(false)
                .frame(egui::Frame::NONE)
                .show(ui, |ui| composer(ui, chat, doc, session, t))
                .inner
        } else {
            false
        };
        conversation(ui, chat, doc, nav, t);
        settings
    })
    .inner
}

fn header(ui: &mut egui::Ui, chat: &mut Chat, t: &theme::Tokens) {
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 2.0 * ui.spacing().item_spacing.x) / 3.0;
        for (task, label) in [(Task::Ask, tl!("Ask")), (Task::Summarize, tl!("Summarize")), (Task::Translate, tl!("Translate"))] {
            let selected = chat.task == task && chat.archive.is_none();
            let button = egui::Button::selectable(selected, label).truncate();
            if ui.add_sized([width.max(1.0), 28.0], button).clicked() {
                chat.task = task;
                chat.archive = None;
            }
        }
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let options = ui.button(tl!("Chat options"));
        egui::Popup::menu(&options).show(|ui| {
            ui.add_enabled_ui(chat.job.is_none(), |ui| {
                if chat.path.is_some() {
                    let mut enabled = chat.history.enabled;
                    if ui.add(crate::widgets::checkbox(&mut enabled, tl!("Save chats on this Mac"))).changed() {
                        let id = chat.revision.clone();
                        let turns = chat.turns.clone();
                        chat.persist(enabled, enabled.then_some((id.as_str(), turns.as_slice())));
                    }
                }
                if ui.button(tl!("New chat")).clicked() {
                    chat.confirm_new = !chat.turns.is_empty();
                    if !chat.confirm_new {
                        chat.archive = None;
                        chat.question.clear();
                    }
                    ui.close();
                }
                if chat.history.revisions.iter().any(|entry| entry.id != chat.revision) {
                    ui.separator();
                    if ui.selectable_label(chat.archive.is_none(), tl!("Current revision")).clicked() {
                        chat.archive = None;
                        ui.close();
                    }
                    for (i, entry) in chat.history.revisions.iter().enumerate().rev().filter(|(_, entry)| entry.id != chat.revision) {
                        let selected = chat.archive.as_deref() == Some(entry.id.as_str());
                        if ui.selectable_label(selected, format!("{} {}", tl!("Older revision"), i + 1)).clicked() {
                            chat.archive = Some(entry.id.clone());
                            ui.close();
                        }
                    }
                    if let Some(id) = chat.archive.clone()
                        && ui.button(tl!("Delete saved chat")).clicked()
                        && chat.persist(chat.history.enabled, Some((&id, &[])))
                    {
                        chat.archive = None;
                        ui.close();
                    }
                }
            });
        });
        if chat.archive.is_some() {
            ui.label(egui::RichText::new(tl!("Earlier revision")).color(t.text_muted));
        }
    });
    if chat.confirm_new {
        ui.label(tl!("Starting a new chat clears this revision’s conversation."));
        ui.horizontal_wrapped(|ui| {
            if ui.button(tl!("Cancel")).clicked() {
                chat.confirm_new = false;
            }
            if ui.button(tl!("Clear and start new chat")).clicked() {
                let id = chat.revision.clone();
                let saved = chat.history.revisions.iter().any(|entry| entry.id == id);
                if !saved || chat.persist(chat.history.enabled, Some((&id, &[]))) {
                    chat.confirm_new = false;
                    chat.archive = None;
                    chat.turns.clear();
                    chat.question.clear();
                }
            }
        });
    }
    ui.separator();
}

fn ask_scope(ui: &mut egui::Ui, chat: &mut Chat, doc: &Document, session: &Session) {
    let choose = ui.button(tl!("Choose PDFs…"));
    egui::Popup::menu(&choose).show(|ui| {
        if ui.button(tl!("Use current PDF")).clicked() {
            chat.clear_selection();
            chat.ask_documents = None;
            ui.close();
        }
        let mut chosen = chat.ask_documents.clone().unwrap_or_else(|| vec![(doc.id, chat.ask_pages)]);
        let mut changed = false;
        egui::ScrollArea::vertical().id_salt("ai-open-pdfs").max_height(230.0).show(ui, |ui| {
            for open in session.docs() {
                let previous = chosen.iter().position(|(id, _)| *id == open.id);
                let mut checked = previous.is_some();
                let enabled = open.allows_copying() && (checked || chosen.len() < ai_ask::MAX_DOCUMENTS);
                let name = ai_ask::document_label(open, session);
                if ui.add_enabled(enabled || checked, crate::widgets::checkbox(&mut checked, &name)).changed() {
                    if checked {
                        chosen.push((open.id, (1, open.info.pages.len())));
                    } else if let Some(index) = previous {
                        chosen.remove(index);
                    }
                    changed = true;
                }
                if let Some((_, (first, last))) = chosen.iter_mut().find(|(id, _)| *id == open.id) {
                    let count = open.info.pages.len().max(1);
                    ui.push_id(open.id.0, |ui| {
                        ui.horizontal(|ui| {
                            let label = ui.label(tl!("From page"));
                            changed |= ui.add(egui::DragValue::new(first).range(1..=count)).labelled_by(label.id).changed();
                            let label = ui.label(tl!("To page"));
                            changed |= ui.add(egui::DragValue::new(last).range(1..=count)).labelled_by(label.id).changed();
                        });
                    });
                }
            }
        });
        if chosen.iter().any(|(id, _)| session.get(*id).is_none()) && ui.button(tl!("Remove closed PDFs")).clicked() {
            chosen.retain(|(id, _)| session.get(*id).is_some());
            changed = true;
        }
        if changed {
            chat.clear_selection();
            chat.ask_documents = Some(chosen);
            chat.error = None;
        }
    });
    if chat.selection.is_some() {
        let name = ai_ask::document_label(doc, session);
        let label = if chat.figure { tl!("Selected area in {name}") } else { tl!("Selected text in {name}") };
        ui.label(crate::i18n::fmt(label, &[("name", &name)]));
    } else if let Some(chosen) = &chat.ask_documents {
        if chosen.is_empty() {
            ui.label(tl!("Choose at least one PDF."));
        }
        egui::ScrollArea::vertical().id_salt("ai-included-pdfs").max_height(72.0).auto_shrink([false, true]).show(ui, |ui| {
            for (id, (first, last)) in chosen {
                if let Some(open) = session.get(*id) {
                    ui.label(format!("{} · {first}–{last}", ai_ask::document_label(open, session)));
                } else {
                    ui.label(tl!("A selected PDF was closed."));
                }
            }
        });
    } else {
        ui.label(ai_ask::document_label(doc, session));
    }
}

fn scan_controls(ui: &mut egui::Ui, chat: &mut Chat) {
    if !chat.figure {
        ui.add(crate::widgets::checkbox(&mut chat.scan_text, tl!("Recognize scanned text locally")));
        if !chat.scan_text {
            return;
        }
    }
    let language_name = ai_ocr::LANGUAGES.iter().find(|(code, _)| *code == chat.ocr_language).map_or(chat.ocr_language, |(_, name)| *name);
    let label = ui.label(tl!("OCR source language"));
    ui.horizontal_wrapped(|ui| {
        craft_ui::ComboBox::from_id_salt("ai-ocr-language")
            .selected_text(tl!(language_name))
            .show_ui(ui, |ui| {
                for &(code, name) in ai_ocr::LANGUAGES {
                    if ui.selectable_value(&mut chat.ocr_language, code, tl!(name)).clicked() {
                        ui.close();
                    }
                }
            })
            .response
            .labelled_by(label.id);
        craft_ui::ComboBox::from_id_salt("ai-ocr-engine").selected_text(tl!(chat.ocr_engine.name())).show_ui(ui, |ui| {
            for engine in [EngineChoice::Auto, EngineChoice::Ocrs, EngineChoice::Tesseract] {
                if ui.selectable_value(&mut chat.ocr_engine, engine, tl!(engine.name())).clicked() {
                    ui.close();
                }
            }
        });
    });
    if chat.figure && chat.task == Task::Ask && chat.destination.as_ref().is_ok_and(ai::Profile::is_llama_cpp) {
        ui.add(crate::widgets::checkbox(&mut chat.vision, tl!("Include image for local vision")));
    }
}

fn composer(ui: &mut egui::Ui, chat: &mut Chat, doc: &Document, session: &Session, t: &theme::Tokens) -> bool {
    let can_edit = chat.job.is_none() && !chat.confirm_new;
    let frame = egui::Frame::new().fill(t.card).stroke(egui::Stroke::new(1.0, t.border)).corner_radius(10).inner_margin(9.0);
    ui.add_space(6.0);
    frame
        .show(ui, |ui| {
            ui.set_width(ui.available_width().max(1.0));
            let mut send_key = false;
            ui.add_enabled_ui(can_edit, |ui| {
                if chat.task == Task::Ask {
                    ask_scope(ui, chat, doc, session);
                }
                if chat.task == Task::Translate
                    && let Some((page, _)) = chat.figure_selection()
                {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("{} · {page}", tl!("Selected area")));
                        if ui.button(tl!("Use page range")).clicked() {
                            chat.clear_selection();
                        }
                    });
                } else if chat.task != Task::Ask || (chat.selection.is_none() && chat.ask_documents.is_none()) {
                    let count = doc.info.pages.len().max(1);
                    let (first, last) = chat.pages_mut();
                    ui.horizontal_wrapped(|ui| {
                        let label = ui.label(tl!("From page"));
                        ui.add(egui::DragValue::new(first).range(1..=count)).labelled_by(label.id);
                        let label = ui.label(tl!("To page"));
                        ui.add(egui::DragValue::new(last).range(1..=count)).labelled_by(label.id);
                    });
                }
                if chat.task != Task::Summarize {
                    scan_controls(ui, chat);
                }
                match chat.task {
                    Task::Ask => {
                        let label = ui.label(tl!("Question"));
                        let field = egui::TextEdit::multiline(&mut chat.question)
                            .hint_text(tl!("Ask about the selected sources…"))
                            .desired_rows(2)
                            .desired_width(ui.available_width())
                            .char_limit(ai_ask::MAX_QUESTION_BYTES);
                        let response = egui::ScrollArea::vertical()
                            .id_salt("question-draft")
                            .max_height(90.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| ui.add(field).labelled_by(label.id))
                            .inner;
                        send_key = response.has_focus() && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
                    }
                    Task::Translate => {
                        let label = ui.label(tl!("Target language"));
                        craft_ui::ComboBox::from_id_salt("translation-language")
                            .width(ui.available_width())
                            .selected_text(chat.language.map_or_else(|| tl!("Choose a language"), |language| tl!(language)))
                            .show_ui(ui, |ui| {
                                for &language in TRANSLATION_LANGUAGES {
                                    if ui.selectable_value(&mut chat.language, Some(language), tl!(language)).clicked() {
                                        ui.close();
                                    }
                                }
                            })
                            .response
                            .labelled_by(label.id);
                    }
                    Task::Summarize => {}
                }
            });
            let mut settings = false;
            if let Ok(profile) = &chat.destination {
                let destination = profile.endpoint().ok();
                let host = destination.as_ref().and_then(|url| url.host_str()).unwrap_or(&profile.base_url);
                let details = format!("{}\n{}", profile.model, destination.as_ref().map_or(profile.base_url.as_str(), |url| url.as_str()));
                ui.add(egui::Label::new(egui::RichText::new(format!("{} · {host}", profile.name)).small().color(t.text_muted)).truncate())
                    .on_hover_text(details);
            }
            ui.horizontal_wrapped(|ui| {
                if chat.destination.is_err() {
                    settings = ui.button(tl!("AI provider settings")).clicked();
                    if ui.button(tl!("Refresh provider")).clicked() {
                        chat.destination = provider();
                    }
                } else if chat.job.is_some() {
                    if chat.stop.load(Ordering::Relaxed) {
                        ui.label(tl!("Stopping…"));
                    } else if ui.button(tl!("Stop")).clicked() {
                        chat.stop.store(true, Ordering::Relaxed);
                    }
                } else {
                    let ready = can_edit
                        && match chat.task {
                            Task::Ask => {
                                !chat.question.trim().is_empty()
                                    && (chat.selection.is_some() || chat.ask_documents.as_ref().is_none_or(|docs| !docs.is_empty()))
                            }
                            Task::Translate => chat.language.is_some(),
                            Task::Summarize => true,
                        };
                    let label = match chat.task {
                        Task::Ask => tl!("Send question"),
                        Task::Summarize => tl!("Send and summarize"),
                        Task::Translate => tl!("Send and translate"),
                    };
                    let button = if ready {
                        egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE)).fill(t.accent).corner_radius(8)
                    } else {
                        egui::Button::new(label)
                    };
                    if ui.add_enabled(ready, button).clicked() || (ready && send_key) {
                        chat.error = chat.send(doc, session, ui.ctx()).err();
                    }
                }
            });
            let disclosure = match &chat.destination {
                Err(error) => error.as_str(),
                Ok(_) if chat.task == Task::Ask && chat.figure && chat.vision => {
                    tl!("Sends the selected image and any OCR labels to the local model.")
                }
                Ok(_) if chat.figure && chat.task != Task::Summarize => tl!("Runs OCR on this device and sends text from the selected area."),
                Ok(_) if chat.task == Task::Ask && chat.selection.is_some() => tl!("Sends your question and the selected text."),
                Ok(_) if chat.task == Task::Ask => tl!("Sends your question, matching excerpts, and recent turns for this scope."),
                Ok(_) => tl!("Sends text from the selected pages."),
            };
            ui.label(egui::RichText::new(disclosure).small().color(t.text_muted));
            settings
        })
        .inner
}

fn conversation(ui: &mut egui::Ui, chat: &Chat, doc: &Document, nav: &mut Option<crate::panels::Nav>, t: &theme::Tokens) {
    let turns = chat
        .archive
        .as_deref()
        .and_then(|id| chat.history.revisions.iter().find(|entry| entry.id == id))
        .map_or(chat.turns.as_slice(), |entry| entry.turns.as_slice());
    egui::ScrollArea::vertical().id_salt(&chat.archive).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
        if turns.is_empty() && chat.pending.is_none() {
            let hint = match chat.task {
                Task::Ask => tl!("Ask a question to find answers with page citations."),
                Task::Summarize => tl!("Get a summary of the selected pages."),
                Task::Translate => tl!("Choose a language to translate text from the selected pages."),
            };
            ui.add_space(16.0);
            ui.label(egui::RichText::new(hint).color(t.text_muted));
        }
        for (index, turn) in turns.iter().enumerate() {
            ui.add_space(10.0);
            prompt_bubble(ui, &turn.question, t);
            ui.add_space(8.0);
            ui.label(&turn.answer);
            if let Some(notice) = &turn.notice {
                ui.label(egui::RichText::new(notice).small().color(t.text_muted));
            }
            if turn.task == Task::Translate && !turn.citations.is_empty() {
                egui::CollapsingHeader::new(tl!("Source text")).id_salt(index).show(ui, |ui| {
                    for cite in &turn.citations {
                        ui.label(format!("{} {}", tl!("Page"), cite.page));
                        ui.label(&cite.excerpt);
                    }
                });
            }
            ui.horizontal_wrapped(|ui| {
                let copy_id = ui.make_persistent_id(("copy-answer", &chat.revision, &chat.archive, index, &turn.question));
                copy_answer(ui, copy_id, &turn.answer, t);
                for cite in &turn.citations {
                    let page = format!("{} {}", tl!("Page"), cite.page);
                    if let Some(location) = &cite.location {
                        let label = format!("{} · {page}", location.source.document);
                        if ui.link(label).on_hover_text(&cite.excerpt).clicked() {
                            *nav = Some(crate::panels::Nav::AiCitation(doc.id, cite.page, location.clone()));
                        }
                    } else if chat.archive.is_none() && (1..=doc.info.pages.len()).contains(&cite.page) {
                        if ui.link(page).on_hover_text(&cite.excerpt).clicked() {
                            *nav = Some(crate::panels::Nav::Page(cite.page - 1));
                        }
                    } else {
                        ui.label(page).on_hover_text(&cite.excerpt);
                    }
                }
            });
        }
        if chat.archive.is_none()
            && let Some(prompt) = &chat.pending
        {
            ui.add_space(10.0);
            prompt_bubble(ui, prompt, t);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tl!("Waiting for an answer…"));
            });
        }
        if let Some(error) = &chat.error {
            ui.add_space(8.0);
            ui.label(error);
        }
    });
}

fn copy_answer(ui: &mut egui::Ui, id: egui::Id, answer: &str, t: &theme::Tokens) {
    const FEEDBACK_SECONDS: f64 = 1.5;
    const ICON_SIZE: f32 = 14.0;
    let now = ui.input(|i| i.time);
    let deadline = ui.data(|data| data.get_temp::<f64>(id));
    let copied = deadline.is_some_and(|deadline| deadline > now);
    if let Some(deadline) = deadline {
        if copied {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(deadline - now));
        } else {
            ui.data_mut(|data| data.remove::<f64>(id));
        }
    }

    let (copy_label, copied_label) = (tl!("Copy"), tl!("Copied"));
    let font = egui::TextStyle::Button.resolve(ui.style());
    let label_width = crate::widgets::text_width(ui, copy_label, &font).max(crate::widgets::text_width(ui, copied_label, &font));
    let width = label_width + ICON_SIZE + ui.spacing().item_spacing.x + 2.0 * ui.spacing().button_padding.x;
    let (icon, label, icon_color, fill) =
        if copied { ("check", copied_label, t.accent_text, t.accent_soft) } else { ("copy", copy_label, t.icon, t.field) };
    let button = egui::Button::image_and_text(crate::widgets::icons::image(icon, ICON_SIZE, icon_color), label)
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, t.border))
        .corner_radius(6)
        .min_size(egui::vec2(width.max(68.0), 26.0));
    if ui.add(button).on_hover_text(tl!("Copy answer")).clicked() {
        ui.ctx().copy_text(answer.to_owned());
        ui.data_mut(|data| data.insert_temp(id, now + FEEDBACK_SECONDS));
        ui.ctx().request_repaint();
    }
}

fn prompt_bubble(ui: &mut egui::Ui, prompt: &str, t: &theme::Tokens) -> egui::Response {
    const PADDING: f32 = 9.0;
    let max_text_width = (ui.available_width() * 0.86 - 2.0 * PADDING).max(1.0);
    let galley = egui::WidgetText::from(prompt).into_galley(ui, Some(egui::TextWrapMode::Wrap), max_text_width, egui::FontSelection::Default);
    let width = galley.size().x + 2.0 * PADDING;
    ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
        ui.allocate_ui_with_layout(egui::vec2(width, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            egui::Frame::new().fill(t.accent_soft).corner_radius(10).inner_margin(PADDING).show(ui, |ui| ui.label(galley)).response
        })
        .inner
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PdfCraftApp, RightPanel};
    use egui_kittest::{Harness, kittest::Queryable};

    #[test]
    fn short_and_wrapping_prompts_share_the_right_edge() {
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 400.0))), ..Default::default() };
        ctx.run_ui(input, |ui| {
            let t = theme::Tokens::for_kind(theme::ThemeKind::Light);
            let space = ui.available_rect_before_wrap();
            let prompts = [
                "What should I prepare before the first review, and when does the project start?",
                "Is there any news about Manama?",
                "Summarize pages 1–2",
                "Translate pages 1–2 to Marathi",
            ];
            let [wrapped, ask, summary, translation] = prompts.map(|prompt| prompt_bubble(ui, prompt, &t).rect);
            for bubble in [wrapped, ask, summary, translation] {
                assert!((bubble.right() - space.right()).abs() <= 1.0, "{bubble:?} was off the right edge of {space:?}");
                assert!(bubble.width() <= space.width() * 0.86 + 1.0, "{bubble:?} exceeds the width limit");
            }
            for bubble in [ask, summary, translation] {
                assert!(bubble.width() < wrapped.width(), "{bubble:?} should fit its text");
                assert!(bubble.height() < wrapped.height(), "{bubble:?} should fit on one line");
            }
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn document_chat_opens_in_a_pane_and_its_citation_navigates() {
        let bytes = pdfcraft_engine::Session::new().create_from_text("Test", "One\u{c}Two").unwrap();
        let mut h = Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_eframe(|_| {
            let mut app = PdfCraftApp::new();
            app.set_option("language", "en").unwrap();
            app.open_bytes("test.pdf", None, bytes.as_ref().clone()).unwrap();
            app.run_command("ai.ask");
            let (_, id) = app.active_ids().unwrap();
            let mut chat = Chat::new(app.session.get(id).unwrap(), 1);
            chat.turns.push(Turn {
                task: Task::Ask,
                question: "What is on the second page?".into(),
                answer: "Two.".into(),
                citations: vec![Citation { page: 2, excerpt: "Two".into(), location: None }],
                scope: (1, 2),
                ask_scope: None,
                notice: None,
            });
            app.ai_chats.insert(id, chat);
            app
        });
        h.run_steps(4);
        assert_eq!(h.state().right, Some(RightPanel::AiChat));
        assert!(h.state().dialog.is_none());
        let citation = h.get_by_role_and_label(egui::accesskit::Role::Link, "Page 2");
        let composer = h.get_by_role_and_label(egui::accesskit::Role::MultilineTextInput, "Question");
        assert!(composer.rect().min.y > citation.rect().max.y && composer.rect().min.y > 650.0);
        let copy_button = h.get_by_role_and_label(egui::accesskit::Role::Button, "Copy");
        let copy_width = copy_button.rect().width();
        copy_button.click();
        let copied = (0..4).any(|_| {
            h.step();
            h.output().platform_output.commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyText(text) if text == "Two."))
        });
        assert!(copied);
        h.step();
        let copied_button = h.get_by_role_and_label(egui::accesskit::Role::Button, "Copied");
        assert!((copied_button.rect().width() - copy_width).abs() <= 1.0);
        h.run_steps(7);
        assert!(h.query_by_label("Copied").is_none());
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Copy");
        h.get_by_role_and_label(egui::accesskit::Role::Link, "Page 2").click();
        h.run_steps(4);
        assert_eq!(h.state().views[0].current, 1);
        for (command, task) in [("ai.summary", Task::Summarize), ("ai.translate", Task::Translate)] {
            h.state_mut().run_command(command);
            h.run_steps(4);
            let (_, id) = h.state().active_ids().unwrap();
            assert!(h.state().ai_chats.get(&id).unwrap().task == task);
            assert_eq!(h.state().right, Some(RightPanel::AiChat));
            assert!(h.state().dialog.is_none());
            assert!(h.query_by_label("Page 2").is_some());
        }
        let language = h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Target language");
        assert!(language.rect().min.y > 650.0);
        language.click();
        h.run_steps(4);
        for option in ["Hindi", "Marathi", "Spanish"] {
            assert!(h.query_by_label(option).is_some());
        }
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Marathi").scroll_to_me();
        h.run_steps(4);
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Marathi").click();
        h.run_steps(4);
        let (_, id) = h.state().active_ids().unwrap();
        assert_eq!(h.state().ai_chats.get(&id).unwrap().language, Some("Marathi"));
        h.state_mut().apply_edit(pdfcraft_engine::Edit::DeletePages { pages: vec![0] });
        h.run_steps(4);
        assert!(h.query_by_label("Page 2").is_none());
    }

    #[test]
    fn ai_ocr_controls_follow_the_selected_area_and_action() {
        let bytes = Session::new().create_blank(300.0, 400.0, 2).unwrap();
        let mut h = Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_eframe(|_| {
            let mut app = PdfCraftApp::new();
            app.set_option("language", "en").unwrap();
            app.open_bytes("figure.pdf", None, bytes.as_ref().clone()).unwrap();
            let (view, id) = app.active_ids().unwrap();
            open_region(&mut app, view, 2, [10.0, 20.0, 90.0, 120.0], Task::Ask);
            app.ai_chats.get_mut(&id).unwrap().destination = Ok(ai::Profile::llama_cpp("Local".into(), ai::LLAMA_CPP_URL.into(), "vision".into()));
            app
        });
        h.run_steps(4);
        h.get_by_label("Selected area in figure.pdf");
        h.get_by_label("OCR source language");
        h.get_by_label("Include image for local vision").click();
        h.run_steps(4);
        h.get_by_label("Sends the selected image and any OCR labels to the local model.");
        h.get_by_label("Translate").click();
        h.run_steps(4);
        h.get_by_label("Selected area · 2");
        assert!(h.query_by_label("Include image for local vision").is_none());
        h.get_by_label("Use page range").click();
        h.run_steps(4);
        h.get_by_label("Recognize scanned text locally").click();
        h.run_steps(4);
        h.get_by_label("OCR source language");
        let (_, id) = h.state().active_ids().unwrap();
        let chat = h.state().ai_chats.get(&id).unwrap();
        assert!(chat.selection.is_none() && chat.scan_text && !chat.vision);
    }

    #[test]
    fn stopping_a_chat_request_discards_its_late_answer() {
        let (reply, job) = std::sync::mpsc::channel();
        let bytes = pdfcraft_engine::Session::new().create_from_text("Test", "One").unwrap();
        let mut h = Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_eframe(|_| {
            let mut app = PdfCraftApp::new();
            app.set_option("language", "en").unwrap();
            app.open_bytes("test.pdf", None, bytes.as_ref().clone()).unwrap();
            app.run_command("ai.ask");
            let (_, id) = app.active_ids().unwrap();
            let chat = app.ai_chats.get_mut(&id).unwrap();
            chat.destination = Ok(ai::Profile::llama_cpp("Local".into(), ai::LLAMA_CPP_URL.into(), "local-model".into()));
            chat.question = "What is on page one?".into();
            chat.pending = Some(chat.question.clone());
            chat.job = Some(job);
            app
        });
        h.run_steps(4);
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Stop").click();
        h.run_steps(4);
        h.get_by_label("Stopping…");
        reply
            .send(Ok((
                None,
                Turn {
                    task: Task::Ask,
                    question: "What is on page one?".into(),
                    answer: "Late answer.".into(),
                    citations: Vec::new(),
                    scope: (1, 1),
                    ask_scope: None,
                    notice: None,
                },
            )))
            .unwrap_or_else(|_| panic!("Chat dropped its pending worker"));
        h.run_steps(4);
        let (_, id) = h.state().active_ids().unwrap();
        let chat = h.state().ai_chats.get(&id).unwrap();
        assert!(chat.job.is_none() && chat.pending.is_none());
        assert!(chat.turns.is_empty());
        assert_eq!(chat.question, "What is on page one?");
    }

    #[test]
    fn ask_picks_open_pdfs_and_highlights_the_cited_same_named_tab() {
        use egui::accesskit::Role;

        let mut h = Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_eframe(|_| {
            let mut app = PdfCraftApp::new();
            app.set_option("language", "en").unwrap();
            for (name, text) in [("paper.pdf", "Sample size was 30."), ("paper.pdf", "Sample size was 60."), ("excluded.pdf", "Sample size was 99.")]
            {
                let bytes = app.session.create_from_text("Test", text).unwrap();
                app.open_bytes(name, None, bytes.as_ref().clone()).unwrap();
            }
            app.active = Some(0);
            app.run_command("ai.ask");
            app
        });
        h.run_steps(5);
        let owner = h.state().views[0].id;
        let target = h.state().views[1].id;
        let target_label = format!("paper.pdf #{}", target.0);
        h.get_by_role_and_label(Role::Button, "Choose PDFs…").click();
        h.run_steps(4);
        h.get_by_label(&target_label).click();
        h.run_steps(4);
        assert_eq!(h.state().ai_chats[&owner].ask_documents, Some(vec![(owner, (1, 1)), (target, (1, 1))]));
        h.key_press(egui::Key::Escape);
        h.run_steps(4);

        let doc = h.state().session.get(target).unwrap();
        let index = ai_ask::Index::build(vec![ai_ask::source(doc, 1, 1).unwrap()], || false).unwrap();
        let hit = index.search("sample size", None)[0];
        let info = &doc.info.pages[0];
        let [x0, y0, x1, y1] = hit.rects[0];
        let [a, b] = info.view_to_user(x0, y0);
        let [c, d] = info.view_to_user(x1, y1);
        let expected = [a.min(c), b.min(d), a.max(c), b.max(d)];
        let source = SourceInfo::new(target.0, target_label.clone(), doc.path.as_deref(), ai_ask::revision_parts(doc));
        let turn = Turn {
            task: Task::Ask,
            question: "What was the sample size?".into(),
            answer: "The second paper reports 60.".into(),
            citations: vec![Citation {
                page: hit.page,
                excerpt: hit.text.clone(),
                location: Some(CitationLocation { source, rects: hit.rects.clone() }),
            }],
            scope: (0, 0),
            ask_scope: None,
            notice: None,
        };
        h.state_mut().ai_chats.get_mut(&owner).unwrap().turns.push(turn);
        h.run_steps(4);
        let link = format!("{target_label} · Page 1");
        h.get_by_role_and_label(Role::Link, &link).click();
        h.run_steps(4);
        assert_eq!(h.state().active, Some(1));
        assert_eq!(h.state().ai_chat_anchor, Some(owner));
        assert_eq!(h.state().views[1].citation_flash.as_ref().unwrap().1, [expected]);
        assert!(h.state().views[0].citation_flash.is_none() && h.state().views[2].citation_flash.is_none());
        h.get_by_role_and_label(Role::Link, &link);
    }
}
