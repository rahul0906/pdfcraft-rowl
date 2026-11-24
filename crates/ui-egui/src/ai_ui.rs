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
    discovering: Option<Job<Vec<String>>>,
}

struct Editor {
    original: Option<Profile>,
    name: String,
    url: String,
    model: String,
    api_format: ApiFormat,
    llama_cpp: bool,
    models: Vec<String>,
    key: Zeroizing<String>,
    clear_key: bool,
}

impl Editor {
    fn new(profile: Profile, editing: bool) -> Self {
        let llama_cpp = profile.is_llama_cpp();
        Self {
            original: editing.then(|| profile.clone()),
            name: profile.name,
            url: profile.base_url,
            model: profile.model,
            api_format: profile.api_format,
            llama_cpp,
            models: Vec::new(),
            key: Zeroizing::new(String::new()),
            clear_key: false,
        }
    }

    fn profile(&self) -> Profile {
        let (name, url, model) = (self.name.trim().into(), self.url.trim().into(), self.model.trim().into());
        if self.llama_cpp { Profile::llama_cpp(name, url, model) } else { Profile::new(name, url, model, self.api_format) }
    }

    fn key(&self) -> Option<&str> {
        if self.clear_key { Some("") } else { (!self.key.is_empty()).then_some(self.key.as_str()) }
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
        if let Some(result) = poll(&mut self.discovering)
            && let Some(editor) = self.editor.as_mut()
        {
            match result {
                Ok(models) => {
                    if let [model] = models.as_slice()
                        && editor.model.trim().is_empty()
                    {
                        editor.model.clone_from(model);
                    }
                    self.status = models.is_empty().then(|| tl!("llama.cpp reported no models.").to_owned());
                    editor.models = models;
                }
                Err(error) => self.status = Some(error),
            }
        }
        let busy = self.testing.is_some() || self.discovering.is_some();
        if let Some(providers) = self.providers.as_ref() {
            let active = providers.active.as_deref().unwrap_or(tl!("Off"));
            ui.label(format!("{} {active}", tl!("Active provider:")));
            ui.add_enabled_ui(!busy, |ui| {
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
                        self.editor =
                            Some(Editor::new(Profile::new("OpenAI".into(), ai::OPENAI_URL.into(), String::new(), ApiFormat::default()), false));
                        self.status = None;
                    }
                    if ui.button(tl!("Add llama.cpp")).clicked() {
                        self.editor = Some(Editor::new(Profile::llama_cpp("llama.cpp".into(), ai::LLAMA_CPP_URL.into(), String::new()), false));
                        self.status = None;
                    }
                    if ui.button(tl!("Add custom provider")).clicked() {
                        self.editor = Some(Editor::new(Profile::new(String::new(), String::new(), String::new(), ApiFormat::default()), false));
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
                ui.add_enabled_ui(!busy, |ui| self.draw_editor(ui));
                if self.discovering.is_some() {
                    ui.spinner();
                    ui.label(tl!("Finding models…"));
                }
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
                    self.editor = Some(Editor::new(profile.clone(), true));
                    self.status = None;
                }
                if ui
                    .button(tl!("Test connection"))
                    .on_hover_text(if profile.is_llama_cpp() {
                        tl!("Sends a sample prompt to the local server.")
                    } else {
                        tl!("Sends a sample prompt to this provider. Provider charges may apply.")
                    })
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
        let saved_key = editor.original.as_ref().is_some_and(Profile::has_key);
        if editor.llama_cpp {
            ui.label(tl!("Connect to a running llama-server on this Mac."));
        }
        egui::Grid::new("ai-profile-fields").num_columns(2).show(ui, |ui| {
            let label = ui.label(tl!("Provider name"));
            ui.add(egui::TextEdit::singleline(&mut editor.name).char_limit(128)).labelled_by(label.id);
            ui.end_row();
            let label = ui.label(tl!("Base URL"));
            if ui.add(egui::TextEdit::singleline(&mut editor.url).char_limit(2048).desired_width(300.0)).labelled_by(label.id).changed() {
                editor.models.clear();
                self.status = None;
            }
            ui.end_row();
            if !editor.llama_cpp {
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
            }
            let label = ui.label(tl!("Model ID"));
            ui.add(egui::TextEdit::singleline(&mut editor.model).char_limit(256)).labelled_by(label.id);
            ui.end_row();
            let label = ui.label(tl!("API key"));
            let mut field = egui::TextEdit::singleline(&mut *editor.key)
                .password(true)
                .char_limit(4096)
                .hint_text(if saved_key { tl!("Saved in Keychain") } else { tl!("Enter key if required") })
                .show(ui);
            if field.response.response.labelled_by(label.id).changed() {
                editor.models.clear();
                self.status = None;
            }
            // The widget's undo history otherwise keeps copies after the editor closes.
            field.state.clear_undoer();
            ui.end_row();
        });
        if saved_key {
            ui.label(tl!("Changing the base URL requires entering the key again."));
            if ui.add(crate::widgets::checkbox(&mut editor.clear_key, tl!("Remove saved key"))).changed() {
                editor.models.clear();
                self.status = None;
            }
        }
        if editor.llama_cpp {
            ui.horizontal_wrapped(|ui| {
                if ui.button(tl!("Find models")).clicked() {
                    editor.models.clear();
                    self.status = None;
                    let profile =
                        editor.original.as_ref().filter(|profile| profile.base_url == editor.url.trim()).cloned().unwrap_or_else(|| editor.profile());
                    let key = editor.key().map(|key| Zeroizing::new(key.to_owned()));
                    match start(ui.ctx(), move || profile.llama_models(key.as_ref().map(|key| key.as_str()))) {
                        Ok(job) => self.discovering = Some(job),
                        Err(error) => self.status = Some(error),
                    }
                }
                if !editor.models.is_empty() {
                    craft_ui::ComboBox::from_id_salt("ai-llama-models").selected_text(tl!("Choose a model")).show_ui(ui, |ui| {
                        for model in &editor.models {
                            ui.selectable_value(&mut editor.model, model.clone(), model);
                        }
                    });
                }
            });
        }
        let (mut save, mut cancel, mut remove) = (false, false, false);
        ui.horizontal_wrapped(|ui| {
            save = ui.button(tl!("Save provider")).clicked();
            cancel = ui.button(tl!("Cancel")).clicked();
            if editor.original.is_some() {
                remove = ui.button(tl!("Remove provider")).clicked();
            }
        });
        let original = editor.original.as_ref().map(|profile| profile.name.as_str());
        if save {
            let profile = editor.profile();
            let name = profile.name.clone();
            let result = ai::save(original, profile, editor.key());
            if result.is_ok() {
                self.editor = None;
                self.selected = Some(name);
            }
            self.update(result);
        } else if remove && let Some(name) = original {
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

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};

    #[test]
    fn llama_cpp_preset_prefills_url_and_applies_model_discovery() {
        let settings = Settings { providers: Some(Providers::default()), ..Default::default() };
        let mut h = Harness::builder().with_size(egui::vec2(760.0, 440.0)).build_ui_state(|ui, settings| settings.draw(ui), settings);
        h.run_steps(3);
        h.get_by_label("Active provider: Off");
        h.get_by_label("Add llama.cpp").click();
        h.run_steps(3);
        let editor = h.state().editor.as_ref().unwrap();
        assert!(editor.llama_cpp && editor.api_format == ApiFormat::ChatCompletions);
        assert_eq!(editor.url, ai::LLAMA_CPP_URL);
        assert!(editor.model.is_empty());
        h.get_by_label("Find models");

        // Deliver the same channel result as the network worker without a socket or Keychain.
        let (send, receive) = mpsc::channel();
        h.state_mut().discovering = Some(receive);
        h.run_steps(3);
        h.get_by_label("Finding models…");
        send.send(Ok(vec!["local-model".into()])).unwrap();
        h.run_steps(3);
        let editor = h.state().editor.as_ref().unwrap();
        assert_eq!(editor.model, "local-model");
        assert_eq!(editor.profile().endpoint().unwrap().as_str(), format!("{}/chat/completions", ai::LLAMA_CPP_URL));
        assert!(h.query_by_label("Finding models…").is_none());
        h.get_by_label("Save provider");
        h.get_by_label("Active provider: Off");
    }
}
