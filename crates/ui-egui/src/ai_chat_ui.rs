//! Multi-turn document chat in the right sidebar.

use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use pdfcraft_engine::{DocId, Document, ai_ask};
use pdfcraft_platform::{
    ai,
    ai_chat::{self, Citation, History, Turn},
};

use crate::ai_ui::{Job, poll, start};

pub(crate) type Chats = HashMap<DocId, Chat>;

type Answer = (Arc<ai_ask::Index>, Turn);

pub(crate) struct Chat {
    generation: u64,
    revision: String,
    path: Option<String>,
    history: History,
    archive: Option<String>,
    confirm_new: bool,
    turns: Vec<Turn>,
    question: String,
    scope: (usize, usize),
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
    fn new(doc: &Document) -> Self {
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
            scope: (1, doc.info.pages.len()),
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

    fn poll_answer(&mut self) {
        if let Some(result) = poll(&mut self.job) {
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            match result {
                Ok((index, turn)) => {
                    self.index = Some((turn.scope, index));
                    if self.turns.len() == ai_chat::MAX_TURNS {
                        self.turns.remove(0);
                    }
                    self.turns.push(turn);
                    self.question.clear();
                    self.save_current();
                }
                Err(error) => self.error = Some(error),
            }
        }
    }

    fn send(&mut self, doc: &Document, ctx: &egui::Context) -> Result<(), String> {
        let latest = provider()?;
        let shown = self.destination.as_ref().map_err(Clone::clone)?;
        if latest.endpoint()? != shown.endpoint()? || latest.model != shown.model || latest.name != shown.name {
            self.destination = Ok(latest);
            return Err("The active provider changed. Review the new destination and ask again.".into());
        }
        let question = self.question.trim().to_owned();
        if question.is_empty() || question.len() > ai_ask::MAX_QUESTION_BYTES {
            return Err("Enter a question of 1–4096 bytes.".into());
        }
        let scope = self.scope;
        let source = ai_ask::source(doc, scope.0, scope.1)?;
        let cached = self.index.as_ref().filter(|(range, _)| *range == scope).map(|(_, index)| index.clone());
        let mut turns: Vec<_> = self.turns.iter().rev().filter(|turn| turn.scope == scope).take(4).cloned().collect();
        turns.reverse();
        self.stop = Arc::default();
        let stop = self.stop.clone();
        self.job = Some(start(ctx, move || {
            let index = match cached {
                Some(index) => index,
                None => Arc::new(ai_ask::Index::build(source, || stop.load(Ordering::Relaxed))?),
            };
            let sources = index
                .search(&question, turns.last().map(|turn| turn.question.as_str()))
                .into_iter()
                .map(|passage| Citation { page: passage.page, excerpt: passage.text.clone() })
                .collect();
            if stop.load(Ordering::Relaxed) {
                return Err("Question stopped.".into());
            }
            let mut turn = ai_chat::ask(&latest, &question, &turns, sources)?;
            turn.scope = scope;
            Ok((index, turn))
        })?);
        Ok(())
    }
}

fn provider() -> Result<ai::Profile, String> {
    ai::load().and_then(|providers| providers.enabled_profile(None).cloned())
}

pub(crate) fn refresh(chats: &mut Chats, session: &pdfcraft_engine::Session) {
    chats.retain(|id, chat| {
        session.get(*id).is_some_and(|doc| doc.edit_generation() == chat.generation && doc.path == chat.path && doc.allows_copying())
    });
    for chat in chats.values_mut() {
        chat.poll_answer();
    }
}

pub(crate) fn panel(ui: &mut egui::Ui, doc: &Document, chats: &mut Chats, nav: &mut Option<crate::panels::Nav>) {
    if let Err(error) = doc.check_copying() {
        ui.label(error.to_string());
        return;
    }
    let chat = chats.entry(doc.id).or_insert_with(|| Chat::new(doc));
    ui.label(&doc.name);
    let can_edit = chat.job.is_none();
    ui.add_enabled_ui(can_edit, |ui| {
        ui.horizontal_wrapped(|ui| {
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
            }
        });
        if chat.confirm_new {
            ui.label(tl!("Starting a new chat clears this revision’s conversation."));
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
        }
        if chat.history.revisions.iter().any(|entry| entry.id != chat.revision) {
            let label = ui.label(tl!("Conversation"));
            craft_ui::ComboBox::from_id_salt("ai-chat-archive")
                .selected_text(if chat.archive.is_none() { tl!("Current revision") } else { tl!("Older revision") })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut chat.archive, None, tl!("Current revision"));
                    for (i, entry) in chat.history.revisions.iter().enumerate().rev().filter(|(_, entry)| entry.id != chat.revision) {
                        ui.selectable_value(&mut chat.archive, Some(entry.id.clone()), format!("{} {}", tl!("Older revision"), i + 1));
                    }
                })
                .response
                .labelled_by(label.id);
        }
        if let Some(id) = chat.archive.clone()
            && ui.button(tl!("Delete saved chat")).clicked()
            && chat.persist(chat.history.enabled, Some((&id, &[])))
        {
            chat.archive = None;
        }
    });
    ui.separator();
    if chat.archive.is_none() {
        if ui.button(tl!("Refresh provider")).clicked() {
            chat.destination = provider();
        }
        match &chat.destination {
            Ok(profile) => {
                ui.label(format!("{} · {}", profile.name, profile.model));
                ui.label(&profile.base_url);
                ui.label(tl!("Ask sends your question, relevant excerpts, and recent turns to this provider."));
            }
            Err(error) => {
                ui.label(error);
            }
        }
        ui.horizontal_wrapped(|ui| {
            let pages = doc.info.pages.len().max(1);
            let first = ui.label(tl!("First page"));
            ui.add_enabled(can_edit, egui::DragValue::new(&mut chat.scope.0).range(1..=pages)).labelled_by(first.id);
            let last = ui.label(tl!("Last page"));
            ui.add_enabled(can_edit, egui::DragValue::new(&mut chat.scope.1).range(1..=pages)).labelled_by(last.id);
        });
        let label = ui.label(tl!("Question"));
        ui.add_enabled(
            can_edit,
            egui::TextEdit::multiline(&mut chat.question).desired_rows(2).desired_width(ui.available_width()).char_limit(ai_ask::MAX_QUESTION_BYTES),
        )
        .labelled_by(label.id);
        if ui.add_enabled(can_edit && chat.destination.is_ok(), egui::Button::new(tl!("Ask"))).clicked() {
            chat.error = chat.send(doc, ui.ctx()).err();
        }
        if chat.job.is_some() {
            ui.spinner();
            if chat.stop.load(Ordering::Relaxed) {
                ui.label(tl!("Stopping…"));
            } else if ui.button(tl!("Stop")).clicked() {
                chat.stop.store(true, Ordering::Relaxed);
            }
        }
    }
    if let Some(error) = &chat.error {
        ui.label(error);
    }
    let turns = chat
        .archive
        .as_deref()
        .and_then(|id| chat.history.revisions.iter().find(|entry| entry.id == id))
        .map_or(chat.turns.as_slice(), |entry| entry.turns.as_slice());
    egui::ScrollArea::vertical().id_salt((doc.id.0, &chat.archive)).stick_to_bottom(true).show(ui, |ui| {
        for turn in turns {
            ui.separator();
            ui.strong(&turn.question);
            ui.label(&turn.answer);
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
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PdfCraftApp, RightPanel};
    use egui_kittest::{Harness, kittest::Queryable};

    #[test]
    fn document_chat_opens_in_a_pane_and_its_citation_navigates() {
        let bytes = pdfcraft_engine::Session::new().create_from_text("Test", "One\u{c}Two").unwrap();
        let mut h = Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_eframe(|_| {
            let mut app = PdfCraftApp::new();
            app.set_option("language", "en").unwrap();
            app.open_bytes("test.pdf", None, bytes.as_ref().clone()).unwrap();
            app.run_command("ai.ask");
            let (_, id) = app.active_ids().unwrap();
            let mut chat = Chat::new(app.session.get(id).unwrap());
            chat.turns.push(Turn {
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
        h.get_by_role_and_label(egui::accesskit::Role::Link, "Page 2").click();
        h.run_steps(4);
        assert_eq!(h.state().views[0].current, 1);
        h.state_mut().apply_edit(pdfcraft_engine::Edit::DeletePages { pages: vec![0] });
        h.run_steps(4);
        assert!(h.query_by_label("Page 2").is_none());
    }
}
