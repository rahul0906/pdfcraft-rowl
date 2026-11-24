use base64::Engine as _;
use pdfcraft_engine::{DocId, ai_ask, ai_compare, ai_ocr, ai_summary::SummarySource};
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

    pub(crate) fn ai_compare(&self, a: &Args) -> Result<Value> {
        let new = self.doc_to_copy(a)?;
        let old = u64::try_from(a.int("other")?)
            .ok()
            .and_then(|id| self.session.get(DocId(id)))
            .ok_or_else(|| Args::wrong("other", "the ID of an open older PDF"))?;
        let mut pages = if a.get("pages").is_some() { self.pages(a, "pages")? } else { Vec::new() };
        pages.sort_unstable();
        pages.dedup();
        let snapshot = ai_compare::Snapshot::new(old, new, &pages).map_err(failed)?;
        let info = |doc: &pdfcraft_engine::Document| {
            SourceInfo::new(doc.id.0, ai_ask::document_label(doc, &self.session), doc.path.as_deref(), ai_ask::revision_parts(doc))
        };
        let (old_info, new_info) = (info(old), info(new));
        let providers = ai::load().map_err(failed)?;
        let profile = providers.enabled_profile(a.opt_str("provider")?).map_err(failed)?;
        let endpoint = profile.endpoint().map_err(failed)?;
        let mut result = json!({
            "provider": profile.name, "endpoint": endpoint.as_str(), "model": profile.model, "sent": false,
            "older": old_info, "newer": new_info, "pages": pages.iter().map(|page| page + 1).collect::<Vec<_>>(),
        });
        result["scope_token"] = json!(ai_chat::revision_id(result.to_string().as_bytes()));
        if !confirmed(a, endpoint.as_str(), &profile.model)? {
            return Ok(result);
        }
        confirm_source_scope(a, &result)?;
        let evidence = snapshot.extract(|| false).map_err(failed)?;
        result["identical_text"] = json!(evidence.changes.is_empty());
        let order = if evidence.changes.is_empty() {
            Vec::new()
        } else {
            result["sent"] = json!(true);
            match ai::rank_changes(profile, &evidence.prompt, evidence.changes.len()) {
                Ok(order) => order,
                Err(error) => {
                    result["notice"] = json!(error);
                    Vec::new()
                }
            }
        };
        result["used_ai"] = json!(!order.is_empty());
        result["changes"] = json!(
            evidence
                .ranked(&order)
                .iter()
                .map(|change| json!({
                    "id": change.id, "kind": change.kind.label().to_lowercase(), "explanation": change.description(),
                    "old": compare_excerpt(change.old.as_ref(), &old_info), "new": compare_excerpt(change.new.as_ref(), &new_info),
                }))
                .collect::<Vec<_>>()
        );
        Ok(result)
    }

    pub(crate) fn ai_transform(&mut self, a: &Args, translate: bool) -> Result<Value> {
        let language = if translate { Some(ai::translation_language(a.str("target_language")?).map_err(ToolError::InvalidArgs)?) } else { None };
        let doc = self.doc_to_copy(a)?;
        let page = |key| usize::try_from(a.int(key)?).map_err(|_| ToolError::InvalidArgs("Page numbers must be positive.".into()));
        let ocr = if translate { scan_options(a)? } else { None };
        let region = if translate { a.get("selection").map(parse_selection).transpose()? } else { None };
        let (first, last, rect) = match region {
            Some(ai_ask::Selection::Rect { page, rect }) if ocr.is_some() && a.get("first_page").is_none() && a.get("last_page").is_none() => {
                (page, page, Some(rect))
            }
            Some(_) => return Err(ToolError::InvalidArgs("Use a selection with ocr_language and omit the page range.".into())),
            None => (page("first_page")?, page("last_page")?, None),
        };
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
        if let Some(options) = &ocr {
            let revision = SourceInfo::new(doc.id.0, doc.name.clone(), doc.path.as_deref(), ai_ask::revision_parts(doc));
            add_scan_preview(&mut result, options, a.get("selection"), &json!(revision));
        }
        if !confirmed(a, endpoint.as_str(), &profile.model)? {
            return Ok(result);
        }
        if let Some(language) = language {
            if let Some(options) = &ocr {
                confirm_source_scope(a, &result)?;
                let scan = source.translation_with_ocr(options, rect, || false).map_err(failed)?;
                result["source_passages"] =
                    json!(scan.passages.iter().map(|(page, text, rects)| json!({"page":page,"text":text,"rects":rects})).collect::<Vec<_>>());
                result["ocr_report"] = scan_report(&scan.report);
                result["translation"] = json!(ai::translate(profile, language, &scan.input).map_err(failed)?);
            } else {
                let text = source.extract_for_translation().map_err(failed)?;
                result["translation"] = json!(ai::translate(profile, language, &text).map_err(failed)?);
            }
        } else {
            let text = source.extract().map_err(failed)?;
            result["summary"] = json!(ai::summarize(profile, &text).map_err(failed)?);
        }
        result["sent"] = json!(true);
        Ok(result)
    }
}

fn compare_excerpt(excerpt: Option<&ai_compare::Excerpt>, source: &SourceInfo) -> Option<Value> {
    let excerpt = excerpt?;
    let citations: Vec<_> = excerpt
        .passages
        .iter()
        .map(|p| Citation {
            page: p.page,
            excerpt: p.text.clone(),
            location: Some(CitationLocation { source: source.clone(), rects: p.rects.clone() }),
        })
        .collect();
    Some(json!({"text": excerpt.text, "before": excerpt.before, "after": excerpt.after, "citations": citations}))
}

fn scan_options(a: &Args) -> Result<Option<ai_ocr::Options>> {
    let language = a.opt_str("ocr_language")?;
    let engine = a.opt_str("ocr_engine")?;
    let Some(language) = language else {
        return if engine.is_none() { Ok(None) } else { Err(Args::missing("ocr_language")) };
    };
    use pdfcraft_engine::ocr::EngineChoice;
    if !ai_ocr::LANGUAGES.iter().any(|(code, _)| *code == language) {
        return Err(Args::wrong("ocr_language", "a supported OCR language code"));
    }
    let engine = match engine.unwrap_or("auto") {
        "auto" => EngineChoice::Auto,
        "ocrs" => EngineChoice::Ocrs,
        "tesseract" => EngineChoice::Tesseract,
        _ => return Err(Args::wrong("ocr_engine", "auto, ocrs or tesseract")),
    };
    Ok(Some(ai_ocr::Options { language: language.into(), engine }))
}

fn scan_report(report: &ai_ocr::Report) -> Value {
    json!({"pages_read":report.pages_read,"low_confidence_words":report.low_confidence_words})
}

fn add_scan_preview(result: &mut Value, options: &ai_ocr::Options, selection: Option<&Value>, revision: &Value) {
    result["ocr_language"] = json!(options.language);
    result["ocr_engine"] = json!(options.engine.name());
    if let Some(selection) = selection {
        result["selection"] = selection.clone();
    }
    let fingerprint = json!({"request":result,"revision":revision});
    result["scope_token"] = json!(ai_chat::revision_id(fingerprint.to_string().as_bytes()));
}

fn confirm_source_scope(a: &Args, result: &Value) -> Result<()> {
    if a.opt_str("confirmed_scope")? != result["scope_token"].as_str() {
        return Err(failed("Confirm the scope token from the preview. Preview again if the selected PDFs, pages or revision changed."));
    }
    Ok(())
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
        let ocr = scan_options(a)?;
        let vision = a.opt_bool("vision")?.unwrap_or(false);
        if vision && (selected.is_none() || ocr.is_none()) {
            return Err(ToolError::InvalidArgs("Vision requires a figure selection and ocr_language.".into()));
        }
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
        let detailed = documents.is_some() || selected.is_some() || ocr.is_some();
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
            let fingerprint = json!({ "documents": documents, "identities": info, "selection": selection,
                "ocr":ocr.as_ref().map(|o| (&o.language,o.engine.name())),"vision":vision });
            result["scope_token"] = json!(ai_chat::revision_id(fingerprint.to_string().as_bytes()));
            result["documents"] = json!(documents);
            if let Some(selection) = selection {
                result["selection"] = selection.clone();
            }
            if let Some(ocr) = &ocr {
                result["ocr_language"] = json!(ocr.language);
                result["ocr_engine"] = json!(ocr.engine.name());
            }
            if vision {
                result["vision"] = json!(true);
            }
        } else {
            result["first_page"] = json!(scope.0);
            result["last_page"] = json!(scope.1);
        }
        if !confirmed(a, endpoint.as_str(), &profile.model)? {
            return Ok(result);
        }
        if detailed {
            confirm_source_scope(a, &result)?;
        }
        if let (true, Some(ai_ask::Selection::Rect { page, rect }), Some(options)) = (vision, selected, &ocr) {
            if !profile.is_llama_cpp() {
                return Err(failed("Choose a local llama.cpp vision profile to include the image."));
            }
            let doc = self.doc_to_copy(a)?;
            let source = SummarySource::new(doc, page, page).map_err(failed)?;
            let png = source.clone().figure_png(rect).map_err(failed)?;
            let (labels, notice) = match source.translation_with_ocr(options, Some(rect), || false) {
                Ok(scan) => {
                    result["ocr_report"] = scan_report(&scan.report);
                    (scan.input, None)
                }
                Err(error) => (String::new(), Some(error)),
            };
            result["answer"] =
                json!(ai::ask_figure(profile, question, &labels, &base64::engine::general_purpose::STANDARD.encode(png)).map_err(failed)?);
            result["sent"] = json!(true);
            result["citations"] = json!([Citation {
                page,
                excerpt: labels,
                location: info.first().and_then(Option::as_ref).cloned().map(|source| CitationLocation { source, rects: vec![rect] })
            }]);
            if let Some(notice) = notice {
                result["ocr_notice"] = json!(notice);
            }
            return Ok(result);
        }
        let index = ai_ask::Index::build_with_ocr(sources, ocr.as_ref(), || false).map_err(failed)?;
        if ocr.is_some() {
            result["ocr_report"] = scan_report(&index.ocr_report);
        }
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
            vec![Turn {
                task: Task::Ask,
                question: previous.into(),
                answer: answer.into(),
                citations: Vec::new(),
                scope,
                ask_scope: None,
                notice: None,
            }]
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
