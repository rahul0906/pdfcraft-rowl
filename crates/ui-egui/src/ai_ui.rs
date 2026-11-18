//! macOS provider preferences and background AI jobs.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use egui::{Context, Ui};
use pdfcraft_platform::ai::{self, ApiFormat, Profile, Providers};
use zeroize::Zeroizing;

pub(crate) type Job<T> = Receiver<Result<T, String>>;

pub(crate) fn start<T: Send + 'static>(ctx: &Context, work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<Job<T>, String> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name("pdfcraft-ai".into())
        .spawn(move || {
            let result = pdfcraft_engine::guard(work).map_err(|_| "The AI request stopped unexpectedly.".to_owned()).and_then(|r| r);
            let _ = tx.send(result);
            ctx.request_repaint();
        })
        .map_err(|_| "Could not start the AI request.".to_owned())?;
    Ok(rx)
}

pub(crate) fn poll<T>(job: &mut Option<Job<T>>) -> Option<Result<T, String>> {
    let result = match job.as_ref()?.try_recv() {
        Ok(result) => result,
        Err(TryRecvError::Empty) => return None,
        Err(TryRecvError::Disconnected) => Err("The AI request stopped unexpectedly.".into()),
    };
    *job = None;
    Some(result)
}

#[derive(Default)]
pub(crate) struct Settings {
    providers: Option<Providers>,
    selected: Option<String>,
    editor: Option<Editor>,
    status: Option<String>,
    testing: Option<Job<()>>,
}

struct Editor {
    original: Option<String>,
    name: String,
    url: String,
    model: String,
    api_format: ApiFormat,
    saved_key: bool,
    key: Zeroizing<String>,
    clear_key: bool,
}

impl Editor {
    fn from(profile: Option<&Profile>, openai: bool) -> Self {
        Self {
            original: profile.map(|p| p.name.clone()),
            name: profile.map_or_else(|| if openai { "OpenAI".into() } else { String::new() }, |p| p.name.clone()),
            url: profile.map_or_else(|| if openai { ai::OPENAI_URL.into() } else { String::new() }, |p| p.base_url.clone()),
            model: profile.map_or_else(String::new, |p| p.model.clone()),
            api_format: profile.map_or(ApiFormat::default(), |p| p.api_format),
            saved_key: profile.is_some_and(Profile::has_key),
            key: Zeroizing::new(String::new()),
            clear_key: false,
        }
    }
}

impl Settings {
    pub(crate) fn reset(&mut self) {
        if self.providers.is_some() || self.editor.is_some() || self.status.is_some() {
            *self = Self::default();
        }
    }

    pub(crate) fn draw(&mut self, ui: &mut Ui) {
        if self.providers.is_none() && self.status.is_none() {
            match ai::load() {
                Ok(providers) => {
                    self.selected = providers.active.clone();
                    self.providers = Some(providers);
                }
                Err(error) => self.status = Some(error),
            }
        }
        if let Some(result) = poll(&mut self.testing) {
            self.status = Some(result.map_or_else(|error| error, |()| tl!("Connection succeeded.").to_owned()));
        }
        if let Some(providers) = self.providers.as_ref() {
            let active = providers.active.as_deref().unwrap_or(tl!("Off"));
            ui.label(format!("{} {active}", tl!("Active provider:")));
            ui.add_enabled_ui(self.testing.is_none(), |ui| {
                ui.horizontal_wrapped(|ui| {
                    let label = ui.label(tl!("Provider profile"));
                    craft_ui::ComboBox::from_id_salt("ai-provider-profile")
                        .selected_text(self.selected.as_deref().unwrap_or(tl!("Choose a profile")))
                        .show_ui(ui, |ui| {
                            for profile in &providers.profiles {
                                if ui.selectable_value(&mut self.selected, Some(profile.name.clone()), &profile.name).changed() {
                                    self.editor = None;
                                    self.status = None;
                                }
                            }
                        })
                        .response
                        .labelled_by(label.id);
                    if ui.button(tl!("Add OpenAI")).clicked() {
                        self.editor = Some(Editor::from(None, true));
                        self.status = None;
                    }
                    if ui.button(tl!("Add custom provider")).clicked() {
                        self.editor = Some(Editor::from(None, false));
                        self.status = None;
                    }
                });
            });
        }
        if self.providers.is_some() {
            if self.testing.is_some() {
                ui.spinner();
                ui.label(tl!("Testing connection…"));
            } else if self.editor.is_some() {
                self.draw_editor(ui);
            } else {
                self.draw_actions(ui);
            }
        }
        if let Some(status) = &self.status {
            ui.label(status);
        }
    }

    fn draw_actions(&mut self, ui: &mut Ui) {
        let Some(providers) = self.providers.as_ref() else { return };
        let profile = providers.profiles.iter().find(|p| Some(p.name.as_str()) == self.selected.as_deref()).cloned();
        if let Some(profile) = &profile {
            ui.label(format!("{} · {}", profile.base_url, profile.model));
        }
        let enabled = providers.active.is_some();
        ui.horizontal_wrapped(|ui| {
            if let Some(profile) = profile {
                if ui.button(tl!("Use provider")).clicked() {
                    self.update(ai::select(Some(&profile.name)));
                }
                if ui.button(tl!("Edit")).clicked() {
                    self.editor = Some(Editor::from(Some(&profile), false));
                    self.status = None;
                }
                if ui
                    .button(tl!("Test connection"))
                    .on_hover_text(tl!("Sends a sample prompt to this provider. Provider charges may apply."))
                    .clicked()
                {
                    self.status = None;
                    match start(ui.ctx(), move || ai::test_connection(&profile)) {
                        Ok(job) => self.testing = Some(job),
                        Err(error) => self.status = Some(error),
                    }
                }
            }
            if enabled && ui.button(tl!("Turn AI off")).clicked() {
                self.update(ai::select(None));
            }
        });
    }

    fn draw_editor(&mut self, ui: &mut Ui) {
        let Some(editor) = self.editor.as_mut() else { return };
        egui::Grid::new("ai-profile-fields").num_columns(2).show(ui, |ui| {
            let label = ui.label(tl!("Provider name"));
            ui.add(egui::TextEdit::singleline(&mut editor.name).char_limit(128)).labelled_by(label.id);
            ui.end_row();
            let label = ui.label(tl!("Base URL"));
            ui.add(egui::TextEdit::singleline(&mut editor.url).char_limit(2048).desired_width(300.0)).labelled_by(label.id);
            ui.end_row();
            let label = ui.label(tl!("API format"));
            let chat = tl!("Chat Completions");
            let responses = tl!("Responses");
            craft_ui::ComboBox::from_id_salt("ai-api-format")
                .selected_text(match editor.api_format {
                    ApiFormat::ChatCompletions => chat,
                    ApiFormat::Responses => responses,
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut editor.api_format, ApiFormat::ChatCompletions, chat);
                    ui.selectable_value(&mut editor.api_format, ApiFormat::Responses, responses);
                })
                .response
                .labelled_by(label.id);
            ui.end_row();
            let label = ui.label(tl!("Model ID"));
            ui.add(egui::TextEdit::singleline(&mut editor.model).char_limit(256)).labelled_by(label.id);
            ui.end_row();
            let label = ui.label(tl!("API key"));
            let mut field = egui::TextEdit::singleline(&mut *editor.key)
                .password(true)
                .char_limit(4096)
                .hint_text(if editor.saved_key { tl!("Saved in Keychain") } else { tl!("Enter key if required") })
                .show(ui);
            field.response.response.labelled_by(label.id);
            // The widget's undo history otherwise keeps copies after the editor closes.
            field.state.clear_undoer();
            ui.end_row();
        });
        if editor.saved_key {
            ui.label(tl!("Changing the base URL requires entering the key again."));
            ui.add(crate::widgets::checkbox(&mut editor.clear_key, tl!("Remove saved key")));
        }
        let (mut save, mut cancel, mut remove) = (false, false, false);
        ui.horizontal_wrapped(|ui| {
            save = ui.button(tl!("Save provider")).clicked();
            cancel = ui.button(tl!("Cancel")).clicked();
            if editor.original.is_some() {
                remove = ui.button(tl!("Remove provider")).clicked();
            }
        });
        if save {
            let name = editor.name.trim().to_owned();
            let profile = Profile::new(name.clone(), editor.url.trim().into(), editor.model.trim().into(), editor.api_format);
            let key = if editor.clear_key { Some("") } else { (!editor.key.is_empty()).then_some(editor.key.as_str()) };
            let result = ai::save(editor.original.as_deref(), profile, key);
            if result.is_ok() {
                self.editor = None;
                self.selected = Some(name);
            }
            self.update(result);
        } else if remove && let Some(name) = editor.original.as_deref() {
            let result = ai::remove(name);
            if result.is_ok() {
                self.editor = None;
                self.selected = None;
            }
            self.update(result);
        } else if cancel {
            self.editor = None;
            self.status = None;
        }
    }

    fn update(&mut self, result: Result<Providers, String>) {
        match result {
            Ok(providers) => {
                self.providers = Some(providers);
                self.status = None;
            }
            Err(error) => self.status = Some(error),
        }
    }
}
