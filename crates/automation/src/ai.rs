use pdfcraft_engine::{ai_ask, ai_summary::SummarySource};
use pdfcraft_platform::{
    ai,
    ai_chat::{self, Citation, CitationLocation, SourceInfo, Task, Turn},
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
        let question = a.str("question")?.trim();
        let previous = a.opt_str("previous_question")?.unwrap_or_default();
        let answer = a.opt_str("previous_answer")?.unwrap_or_default();
        if question.is_empty() || [question, previous, answer].iter().any(|s| s.len() > ai_ask::MAX_QUESTION_BYTES) {
            return Err(ToolError::InvalidArgs("Enter a question and keep each conversation field within 4096 bytes.".into()));
        }
        let selected = a.get("selection").map(parse_selection).transpose()?;
        if selected.is_some() && !(previous.is_empty() && answer.is_empty()) {
            return Err(ToolError::InvalidArgs("For a selection-only question, omit previous conversation fields.".into()));
        }
        let ranged = a.get("first_page").is_some() || a.get("last_page").is_some();
        let documents = a.get("documents");
        if (documents.is_some() && (a.get("doc").is_some() || selected.is_some() || ranged)) || (selected.is_some() && ranged) {
            return Err(ToolError::InvalidArgs(
                "Choose documents with optional per-file ranges, or use doc with a page range or text selection.".into(),
            ));
        }
        let detailed = documents.is_some() || selected.is_some();
        let sources = if let Some(documents) = documents {
            let docs = documents
                .as_array()
                .filter(|v| !v.is_empty() && v.len() <= ai_ask::MAX_DOCUMENTS)
                .ok_or_else(|| Args::wrong("documents", "an array of 1 to 6 open PDFs"))?;
            docs.iter()
                .map(|value| {
                    let fields = value.as_object().ok_or_else(|| Args::wrong("documents", "an array of document scopes"))?;
                    if fields.keys().any(|key| !matches!(key.as_str(), "doc" | "first_page" | "last_page")) {
                        return Err(ToolError::InvalidArgs("Document scopes accept doc, first_page and last_page.".into()));
                    }
                    let args = Args(value);
                    let doc = self.doc_to_copy(&args)?;
                    Ok((doc, ask_source(doc, &args)?))
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            let doc = self.doc_to_copy(a)?;
            let source = match selected {
                Some(selection) => ai_ask::selected_source(doc, selection).map_err(failed)?,
                None => ask_source(doc, a)?,
            };
            vec![(doc, source)]
        };
        let (info, sources) = sources
            .into_iter()
            .map(|(doc, source)| {
                let info = detailed
                    .then(|| SourceInfo::new(doc.id.0, ai_ask::document_label(doc, &self.session), doc.path.as_deref(), source.revision_parts()));
                (info, source)
            })
            .unzip::<_, _, Vec<_>, Vec<_>>();
        ai_ask::validate_scope(&sources).map_err(failed)?;
        let providers = ai::load().map_err(failed)?;
        let profile = providers.enabled_profile(a.opt_str("provider")?).map_err(failed)?;
        let endpoint = profile.endpoint().map_err(failed)?;
        let mut result = json!({
            "endpoint": endpoint.as_str(), "model": profile.model, "provider": profile.name, "question": question, "sent": false
        });
        let scope = sources.first().map(ai_ask::Source::pages).ok_or_else(|| failed("Select an open PDF."))?;
        if detailed {
            let documents: Vec<_> = info
                .iter()
                .zip(&sources)
                .filter_map(|(info, source)| {
                    info.as_ref().map(|info| {
                        let (first, last) = source.pages();
                        json!({ "doc": info.doc, "document": info.document, "revision": info.revision, "first_page": first, "last_page": last })
                    })
                })
                .collect();
            let selection = a.get("selection");
            let fingerprint = json!({ "documents": documents, "identities": info, "selection": selection });
            result["scope_token"] = json!(ai_chat::revision_id(fingerprint.to_string().as_bytes()));
            result["documents"] = json!(documents);
            if let Some(selection) = selection {
                result["selection"] = selection.clone();
            }
        } else {
            result["first_page"] = json!(scope.0);
            result["last_page"] = json!(scope.1);
        }
        if !confirmed(a, endpoint.as_str(), &profile.model)? {
            return Ok(result);
        }
        if detailed && a.opt_str("confirmed_scope")? != result["scope_token"].as_str() {
            return Err(failed("Confirm the scope token from the preview. Preview again if the selected PDFs, pages or revision changed."));
        }
        let index = ai_ask::Index::build(sources, || false).map_err(failed)?;
        let hits: Vec<_> = if selected.is_some() { index.selection().collect() } else { index.search(question, Some(previous)) };
        let sources: Vec<_> = hits
            .into_iter()
            .map(|hit| Citation {
                page: hit.page,
                excerpt: hit.text.clone(),
                location: info
                    .iter()
                    .flatten()
                    .find(|source| source.doc == hit.doc.0)
                    .cloned()
                    .map(|source| CitationLocation { source, rects: hit.rects.clone() }),
            })
            .collect();
        let sent = !sources.is_empty();
        let history = if previous.is_empty() {
            Vec::new()
        } else {
            vec![Turn { task: Task::Ask, question: previous.into(), answer: answer.into(), citations: Vec::new(), scope, ask_scope: None }]
        };
        let turn = ai_chat::ask(profile, question, &history, sources).map_err(failed)?;
        result["sent"] = json!(sent);
        result["answer"] = json!(turn.answer);
        result["citations"] = json!(turn.citations);
        Ok(result)
    }
}

fn ask_source(doc: &pdfcraft_engine::Document, a: &Args) -> Result<ai_ask::Source> {
    let page = |key, default| a.opt_int(key)?.map_or(Ok(default), |n| usize::try_from(n).map_err(|_| Args::wrong(key, "a positive page number")));
    ai_ask::source(doc, page("first_page", 1)?, page("last_page", doc.info.pages.len())?).map_err(failed)
}

fn parse_selection(value: &Value) -> Result<ai_ask::Selection> {
    let fields = value.as_object().ok_or_else(|| Args::wrong("selection", "a page and rectangle"))?;
    if fields.keys().any(|key| !matches!(key.as_str(), "page" | "rect")) {
        return Err(Args::wrong("selection", "a page and rectangle"));
    }
    let page = Args(value).int("page")?.try_into().map_err(|_| Args::wrong("page", "a positive page number"))?;
    let rect = fields
        .get("rect")
        .and_then(Value::as_array)
        .and_then(|rect| <&[Value; 4]>::try_from(rect.as_slice()).ok())
        .ok_or_else(|| Args::wrong("rect", "four coordinates in displayed page points"))?;
    let [Some(x0), Some(y0), Some(x1), Some(y1)] = rect.each_ref().map(Value::as_f64) else {
        return Err(Args::wrong("rect", "four coordinates"));
    };
    Ok(ai_ask::Selection::Rect { page, rect: [x0 as f32, y0 as f32, x1 as f32, y1 as f32] })
}
