use pdfcraft_engine::{ai_ask, ai_summary::SummarySource};
use pdfcraft_platform::{
    ai,
    ai_chat::{self, Citation, Task, Turn},
};
use serde_json::{Value, json};

use crate::Automation;
use crate::error::{Result, ToolError, failed};
use crate::json::Args;

impl Automation {
    pub(crate) fn ai_llama_models(&self, a: &Args) -> Result<Value> {
        let base_url = a.opt_str("base_url")?;
        let profile = if let Some(name) = a.opt_str("provider")? {
            if base_url.is_some() {
                return Err(ToolError::InvalidArgs("Use either a saved provider or a base URL.".into()));
            }
            let providers = ai::load().map_err(failed)?;
            providers.profiles.into_iter().find(|p| p.name == name).ok_or_else(|| failed("The selected AI provider no longer exists."))?
        } else {
            ai::Profile::llama_cpp("llama.cpp".into(), base_url.unwrap_or(ai::LLAMA_CPP_URL).into(), String::new())
        };
        Ok(json!({ "models": profile.llama_models(None).map_err(failed)? }))
    }

    pub(crate) fn ai_transform(&mut self, a: &Args, translate: bool) -> Result<Value> {
        let language = if translate { Some(ai::translation_language(a.str("target_language")?).map_err(ToolError::InvalidArgs)?) } else { None };
        let doc = self.doc_to_copy(a)?;
        let page = |key| usize::try_from(a.int(key)?).map_err(|_| ToolError::InvalidArgs("Page numbers must be positive.".into()));
        let (first, last) = (page("first_page")?, page("last_page")?);
        let source =
            if translate { SummarySource::for_translation(doc, first, last) } else { SummarySource::new(doc, first, last) }.map_err(failed)?;
        let providers = ai::load().map_err(failed)?;
        let profile = providers.enabled_profile(a.opt_str("provider")?).map_err(failed)?;
        let endpoint = profile.endpoint().map_err(failed)?;
        let mut result = json!({
            "provider": profile.name, "endpoint": endpoint.as_str(), "model": profile.model,
            "first_page": first, "last_page": last, "sent": false
        });
        if let Some(language) = language {
            result["target_language"] = json!(language);
        }
        if !confirmed(a, endpoint.as_str(), &profile.model)? {
            return Ok(result);
        }
        if let Some(language) = language {
            let text = source.extract_for_translation().map_err(failed)?;
            result["translation"] = json!(ai::translate(profile, language, &text).map_err(failed)?);
        } else {
            let text = source.extract().map_err(failed)?;
            result["summary"] = json!(ai::summarize(profile, &text).map_err(failed)?);
        }
        result["sent"] = json!(true);
        Ok(result)
    }
}

fn confirmed(a: &Args, endpoint: &str, model: &str) -> Result<bool> {
    if !a.opt_bool("user_confirmed")?.unwrap_or(false) {
        return Ok(false);
    }
    if a.opt_str("confirmed_endpoint")? != Some(endpoint) || a.opt_str("confirmed_model")? != Some(model) {
        return Err(failed("The endpoint and model must match the preview. Preview again and confirm the destination before sending."));
    }
    Ok(true)
}

impl Automation {
    pub(crate) fn ai_ask(&mut self, a: &Args) -> Result<Value> {
        let doc = self.doc_to_copy(a)?;
        let page = |key, default| {
            a.opt_int(key)?.map_or(Ok(default), |n| usize::try_from(n).map_err(|_| ToolError::InvalidArgs("Page numbers must be positive.".into())))
        };
        let (first, last) = (page("first_page", 1)?, page("last_page", doc.info.pages.len())?);
        let question = a.str("question")?.trim();
        let previous = a.opt_str("previous_question")?.unwrap_or_default();
        let answer = a.opt_str("previous_answer")?.unwrap_or_default();
        if question.is_empty() || [question, previous, answer].iter().any(|s| s.len() > ai_ask::MAX_QUESTION_BYTES) {
            return Err(ToolError::InvalidArgs("Enter a question and keep each conversation field within 4096 bytes.".into()));
        }
        let source = ai_ask::source(doc, first, last).map_err(failed)?;
        let providers = ai::load().map_err(failed)?;
        let profile = providers.enabled_profile(a.opt_str("provider")?).map_err(failed)?;
        let endpoint = profile.endpoint().map_err(failed)?;
        let mut result = json!({
            "endpoint": endpoint.as_str(), "model": profile.model, "provider": profile.name,
            "first_page": first, "last_page": last, "question": question, "sent": false
        });
        if !confirmed(a, endpoint.as_str(), &profile.model)? {
            return Ok(result);
        }
        let index = ai_ask::Index::build(source, || false).map_err(failed)?;
        let hits = index.search(question, Some(previous));
        let sources: Vec<_> = hits.into_iter().map(|hit| Citation { page: hit.page, excerpt: hit.text.clone() }).collect();
        let sent = !sources.is_empty();
        let history = if previous.is_empty() {
            Vec::new()
        } else {
            vec![Turn { task: Task::Ask, question: previous.into(), answer: answer.into(), citations: Vec::new(), scope: (first, last) }]
        };
        let turn = ai_chat::ask(profile, question, &history, sources).map_err(failed)?;
        result["sent"] = json!(sent);
        result["answer"] = json!(turn.answer);
        result["citations"] = json!(turn.citations);
        Ok(result)
    }
}
