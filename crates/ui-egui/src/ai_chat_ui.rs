//! Multi-turn document chat in the right sidebar.

use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use pdfcraft_engine::{
    DocId, Document, ai_ask,
    ai_summary::{self, SummarySource},
};
use pdfcraft_platform::{
    ai,
    ai_chat::{self, Citation, History, Task, Turn},
};

use crate::ai_ui::{Job, poll, start};
use crate::{PdfCraftApp, RightPanel, theme};

pub(crate) type Chats = HashMap<DocId, Chat>;

type Answer = (Option<Arc<ai_ask::Index>>, Turn);

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
    summary_pages: (usize, usize),
    translation_pages: (usize, usize),
    language: Option<&'static str>,
    pending: Option<String>,
    index: Option<((usize, usize), Arc<ai_ask::Index>)>,
    job: Option<Job<Answer>>,
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
            summary_pages: (1, doc.info.pages.len().min(ai_summary::MAX_PAGES)),
            translation_pages: (current_page, current_page),
            language: None,
            pending: None,
            index: None,
            job: None,
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
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            match result {
                Ok((index, turn)) => {
                    if let Some(index) = index {
                        self.index = Some((turn.scope, index));
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

    fn send(&mut self, doc: &Document, ctx: &egui::Context) -> Result<(), String> {
        let latest = provider()?;
        let shown = self.destination.as_ref().map_err(Clone::clone)?;
        if latest.endpoint()? != shown.endpoint()? || latest.model != shown.model || latest.name != shown.name {
            self.destination = Ok(latest);
            return Err("The active provider changed. Review the new destination and send again.".into());
        }
        let task = self.task;
        let scope = *self.pages_mut();
        let source = match task {
            Task::Ask => ai_ask::source(doc, scope.0, scope.1)?,
            Task::Summarize => SummarySource::new(doc, scope.0, scope.1)?,
            Task::Translate => SummarySource::for_translation(doc, scope.0, scope.1)?,
        };
        let language = self.language;
        let page_range = format!("{}–{}", scope.0, scope.1);
        let question = match task {
            Task::Ask => {
                let question = self.question.trim();
                if question.is_empty() || question.len() > ai_ask::MAX_QUESTION_BYTES {
                    return Err("Enter a question of 1–4096 bytes.".into());
                }
                question.to_owned()
            }
            Task::Summarize => crate::i18n::fmt(tl!("Summarize pages {pages}"), &[("pages", &page_range)]),
            Task::Translate => {
                let target = language.ok_or("Choose a target language.")?;
                crate::i18n::fmt(tl!("Translate pages {pages} to {language}"), &[("pages", &page_range), ("language", tl!(target))])
            }
        };
        let cached = self.index.as_ref().filter(|(range, _)| *range == scope).map(|(_, index)| index.clone());
        let mut turns: Vec<_> = if task == Task::Ask {
            self.turns.iter().rev().filter(|turn| turn.task == Task::Ask && turn.scope == scope).take(4).cloned().collect()
        } else {
            Vec::new()
        };
        turns.reverse();
        self.stop = Arc::default();
        let stop = self.stop.clone();
        let prompt = question.clone();
        self.job = Some(start(ctx, move || {
            let check_stop = || {
                if stop.load(Ordering::Relaxed) { Err("Request stopped.".to_owned()) } else { Ok(()) }
            };
            check_stop()?;
            if task == Task::Ask {
                let index = match cached {
                    Some(index) => index,
                    None => Arc::new(ai_ask::Index::build(source, || stop.load(Ordering::Relaxed))?),
                };
                let sources = index
                    .search(&question, turns.last().map(|turn| turn.question.as_str()))
                    .into_iter()
                    .map(|passage| Citation { page: passage.page, excerpt: passage.text.clone() })
                    .collect();
                check_stop()?;
                let mut turn = ai_chat::ask(&latest, &question, &turns, sources)?;
                turn.scope = scope;
                return Ok((Some(index), turn));
            }
            let answer = if task == Task::Summarize {
                let text = source.extract()?;
                check_stop()?;
                ai::summarize(&latest, &text)?
            } else {
                let text = source.extract_for_translation()?;
                check_stop()?;
                ai::translate(&latest, language.ok_or("Choose a target language.")?, &text)?
            };
            Ok((None, Turn { task, question, answer, citations: Vec::new(), scope }))
        })?);
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
    chat.destination = provider();
    app.choose_right_panel(Some(RightPanel::AiChat));
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
        chat.poll_answer();
    }
}

pub(crate) fn panel(
    ui: &mut egui::Ui,
    t: &theme::Tokens,
    doc: &Document,
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
                .show(ui, |ui| composer(ui, chat, doc, t))
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

fn composer(ui: &mut egui::Ui, chat: &mut Chat, doc: &Document, t: &theme::Tokens) -> bool {
    let can_edit = chat.job.is_none() && !chat.confirm_new;
    let frame = egui::Frame::new().fill(t.card).stroke(egui::Stroke::new(1.0, t.border)).corner_radius(10).inner_margin(9.0);
    ui.add_space(6.0);
    frame
        .show(ui, |ui| {
            ui.set_width(ui.available_width().max(1.0));
            let mut send_key = false;
            ui.add_enabled_ui(can_edit, |ui| {
                let count = doc.info.pages.len().max(1);
                let (first, last) = chat.pages_mut();
                ui.horizontal_wrapped(|ui| {
                    let label = ui.label(tl!("From page"));
                    ui.add(egui::DragValue::new(first).range(1..=count)).labelled_by(label.id);
                    let label = ui.label(tl!("To page"));
                    ui.add(egui::DragValue::new(last).range(1..=count)).labelled_by(label.id);
                });
                match chat.task {
                    Task::Ask => {
                        let label = ui.label(tl!("Question"));
                        let field = egui::TextEdit::multiline(&mut chat.question)
                            .hint_text(tl!("Ask about this PDF…"))
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
                            Task::Ask => !chat.question.trim().is_empty(),
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
                        chat.error = chat.send(doc, ui.ctx()).err();
                    }
                }
            });
            let disclosure = match &chat.destination {
                Err(error) => error.as_str(),
                Ok(_) if chat.task == Task::Ask => tl!("Sends your question, matching excerpts, and recent turns."),
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
            ui.horizontal_wrapped(|ui| {
                let copy_id = ui.make_persistent_id(("copy-answer", &chat.revision, &chat.archive, index, &turn.question));
                copy_answer(ui, copy_id, &turn.answer, t);
                for cite in &turn.citations {
                    let label = format!("{} {}", tl!("Page"), cite.page);
                    if chat.archive.is_none() && (1..=doc.info.pages.len()).contains(&cite.page) {
                        if ui.link(label).on_hover_text(&cite.excerpt).clicked() {
                            *nav = Some(crate::panels::Nav::Page(cite.page - 1));
                        }
                    } else {
                        ui.label(label).on_hover_text(&cite.excerpt);
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
                citations: vec![Citation { page: 2, excerpt: "Two".into() }],
                scope: (1, 2),
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
}
