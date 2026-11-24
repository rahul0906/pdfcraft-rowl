use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use pdfcraft_engine::{DocId, Session, ai_ask, ai_compare};
use pdfcraft_platform::ai::{self, Profile};

use crate::ai_ui::{Job, poll, start};

use super::PanelAction;

pub(super) struct Explanation {
    revisions: Option<[ai_ask::Stamp; 2]>,
    open: bool,
    profile: Result<Profile, String>,
    job: Option<Job<Outcome>>,
    stop: Arc<AtomicBool>,
    outcome: Option<Outcome>,
    status: Option<String>,
}

struct Outcome {
    changes: Vec<ai_compare::Change>,
    notice: Option<String>,
}

impl Drop for Explanation {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Explanation {
    pub(super) fn new(session: &Session, old: DocId, new: DocId) -> Self {
        let revisions = session.get(old).zip(session.get(new)).map(|(old, new)| [ai_ask::stamp(old), ai_ask::stamp(new)]);
        Self { revisions, open: false, profile: Err(String::new()), job: None, stop: Arc::default(), outcome: None, status: None }
    }

    pub(super) fn matches(&self, session: &Session) -> bool {
        self.revisions.as_ref().is_some_and(|stamps| stamps.iter().all(|stamp| session.get(stamp.doc).is_some_and(|doc| stamp.matches(doc))))
    }

    pub(super) fn refresh(&mut self, session: &Session) {
        if !self.matches(session) {
            self.stop.store(true, Ordering::Relaxed);
            self.job = None;
            self.outcome = None;
        } else if let Some(result) = poll(&mut self.job) {
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            match result {
                Ok(outcome) => self.outcome = Some(outcome),
                Err(error) => self.status = Some(error),
            }
        }
    }

    pub(super) fn draw(&mut self, ui: &mut egui::Ui, session: &Session, pages: &[usize]) -> Option<PanelAction> {
        if !self.open {
            let available = self.matches(session);
            let button = ui.add_enabled(available, egui::Button::new(tl!("Explain what changed")));
            if !available {
                button.on_disabled_hover_text(tl!("Compare the current versions again."));
            } else if button.clicked() {
                self.open = true;
                self.profile = active_profile();
            }
            return None;
        }
        ui.separator();
        if !self.matches(session) {
            ui.label(tl!("A source PDF changed or closed. Compare the current versions again."));
            return None;
        }
        if self.job.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tl!("Reviewing text changes…"));
                if ui.button(tl!("Stop")).clicked() {
                    self.stop.store(true, Ordering::Relaxed);
                    self.job = None;
                }
            });
            return None;
        }
        if let Some(outcome) = &self.outcome {
            return show_outcome(ui, outcome);
        }

        let [older, newer] = self.revisions.as_ref()?;
        let old = session.get(older.doc)?;
        let new = session.get(newer.doc)?;
        ui.label(crate::i18n::fmt(tl!("Old file: {name}"), &[("name", &ai_ask::document_label(old, session))]));
        ui.label(crate::i18n::fmt(tl!("New file: {name}"), &[("name", &ai_ask::document_label(new, session))]));
        match &self.profile {
            Err(error) => {
                ui.label(error);
                let mut settings = false;
                ui.horizontal_wrapped(|ui| {
                    settings = ui.button(tl!("AI provider settings")).clicked();
                    if ui.button(tl!("Refresh provider")).clicked() {
                        self.profile = active_profile();
                    }
                });
                if settings {
                    return Some(PanelAction::ExplainSettings);
                }
            }
            Ok(profile) => {
                let endpoint = profile.endpoint().map(|url| url.to_string()).unwrap_or_default();
                ui.label(format!("{} · {}", profile.name, profile.model)).on_hover_text(endpoint);
                ui.label(tl!("Sends changed text and nearby wording to this provider to rank the changes."));
                if ui.button(tl!("Send and explain")).clicked() {
                    self.status = self.send(ui.ctx(), session, old, new, pages).err();
                }
            }
        }
        if let Some(status) = &self.status {
            ui.label(status);
        }
        None
    }

    fn send(
        &mut self,
        ctx: &egui::Context,
        session: &Session,
        old: &pdfcraft_engine::Document,
        new: &pdfcraft_engine::Document,
        pages: &[usize],
    ) -> Result<(), String> {
        if !self.matches(session) {
            return Err("A source PDF changed or closed. Compare the current versions again.".into());
        }
        let current = active_profile()?;
        let shown = self.profile.as_ref().map_err(Clone::clone)?;
        if current.endpoint()? != shown.endpoint()? || current.model != shown.model || current.name != shown.name {
            self.profile = Ok(current);
            return Err("The active provider changed. Review the new destination and send again.".into());
        }
        let snapshot = ai_compare::Snapshot::new(old, new, pages)?;
        self.stop = Arc::default();
        let stop = self.stop.clone();
        self.job = Some(start(ctx, move || {
            let stopped = || stop.load(Ordering::Relaxed);
            let evidence = snapshot.extract(stopped)?;
            if stopped() {
                return Err("Request stopped.".into());
            }
            if evidence.changes.is_empty() {
                return Ok(Outcome { changes: Vec::new(), notice: None });
            }
            let (order, notice) = match ai::rank_changes(&current, &evidence.prompt, evidence.changes.len()) {
                Ok(order) => (order, None),
                Err(error) => (Vec::new(), Some(error)),
            };
            Ok(Outcome { changes: evidence.ranked(&order), notice })
        })?);
        Ok(())
    }
}

fn active_profile() -> Result<Profile, String> {
    ai::load().and_then(|profiles| profiles.enabled_profile(None).cloned())
}

fn show_outcome(ui: &mut egui::Ui, outcome: &Outcome) -> Option<PanelAction> {
    ui.label(egui::RichText::new(tl!("Explanation")).strong());
    if let Some(notice) = &outcome.notice {
        ui.label(egui::RichText::new(notice).small().color(ui.visuals().weak_text_color()));
    }
    if outcome.changes.is_empty() {
        ui.label(tl!("No differences in the text."));
    }
    let mut action = None;
    for change in &outcome.changes {
        ui.add_space(5.0);
        ui.label(egui::RichText::new(change.description()).strong());
        for (old, side) in [(true, &change.old), (false, &change.new)] {
            let Some(side) = side else { continue };
            let version = if old { tl!("Older") } else { tl!("Newer") };
            let mut phrase = egui::text::LayoutJob::default();
            let font = egui::TextStyle::Body.resolve(ui.style());
            let muted = egui::TextFormat::simple(font.clone(), ui.visuals().weak_text_color());
            phrase.append(&format!("{version} · {} ", side.before), 0.0, muted.clone());
            phrase.append(&side.text, 0.0, egui::TextFormat::simple(font, ui.visuals().hyperlink_color));
            if !side.after.is_empty() {
                phrase.append(&format!(" {}", side.after), 0.0, muted);
            }
            ui.label(phrase);
            ui.horizontal_wrapped(|ui| {
                for passage in &side.passages {
                    let label = if old { tl!("Older page {page}") } else { tl!("Newer page {page}") };
                    let label = crate::i18n::fmt(label, &[("page", &passage.page.to_string())]);
                    if ui.link(label).on_hover_text(&passage.text).clicked() {
                        action = Some(PanelAction::ExplainCitation { old, page: passage.page - 1, rects: passage.rects.clone() });
                    }
                }
            });
        }
    }
    ui.separator();
    action
}

#[cfg(test)]
mod tests {
    use egui_kittest::{Harness, kittest::Queryable};

    use super::*;
    use crate::PdfCraftApp;

    #[test]
    fn ai_compare_citations_reveal_the_correct_version_and_expire_when_it_closes() {
        let session = Session::new();
        let old = session.create_from_text("Old", "Unchanged page\u{c}The sample size was 100.").unwrap();
        let new = session.create_from_text("New", "Unchanged page\u{c}The sample size was 120.").unwrap();
        let mut h = Harness::builder().with_size(egui::vec2(1400.0, 1000.0)).build_eframe(|cc| {
            PdfCraftApp::install_runtime(&cc.egui_ctx);
            let mut app = PdfCraftApp::for_unit_tests();
            app.set_option(&cc.egui_ctx, "language", "en").unwrap();
            app.open_bytes(&cc.egui_ctx, "old.pdf", None, old.as_ref().clone()).unwrap();
            app.open_bytes(&cc.egui_ctx, "new.pdf", None, new.as_ref().clone()).unwrap();
            app.compare_old = Some(app.views[0].id);
            app.active = Some(1);
            app.run_compare(&cc.egui_ctx);
            app
        });
        h.ctx.options_mut(|options| options.warn_on_id_clash = true);
        h.run_steps(4);
        h.get_by_label("Explain what changed").click();
        h.run_steps(4);
        let app = h.state_mut();
        let state = app.compare.as_mut().unwrap();
        assert!(state.explanation.open);
        let snapshot = ai_compare::Snapshot::new(app.session.get(state.old).unwrap(), app.session.get(state.new).unwrap(), &state.pages).unwrap();
        state.explanation.outcome = Some(Outcome { changes: snapshot.extract(|| false).unwrap().changes, notice: None });
        state.side_by_side = false;
        h.run_steps(4);
        h.get_by_role_and_label(egui::accesskit::Role::Link, "Older page 2").click();
        h.run_steps(4);
        let app = h.state();
        assert!(app.compare.as_ref().unwrap().side_by_side);
        assert_eq!(app.views[0].current(), 1);
        assert!(app.views[0].session.citation_flash.as_ref().is_some_and(|(page, rects, _)| *page == 1 && !rects.is_empty()));
        assert!(
            !h.output().shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text)
            if text.galley.text().contains("use of widget ID"))),
            "side-by-side comparison must not duplicate the shared quick tools"
        );
        h.get_by_role_and_label(egui::accesskit::Role::Link, "Newer page 2").click();
        h.run_steps(4);
        assert_eq!(h.state().views[1].current(), 1);
        assert!(h.state().views[1].session.citation_flash.as_ref().is_some_and(|(page, rects, _)| *page == 1 && !rects.is_empty()));

        h.state_mut().close_tab(0);
        h.run_steps(4);
        assert!(h.query_by_label("Older page 2").is_none());
        assert!(h.state().compare.as_ref().unwrap().explanation.outcome.is_none());
    }
}
