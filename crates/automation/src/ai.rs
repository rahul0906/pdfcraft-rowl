use pdfcraft_engine::ai_summary::SummarySource;
use pdfcraft_platform::ai;
use serde_json::{Value, json};

use crate::Automation;
use crate::error::{Result, ToolError, failed};
use crate::json::Args;

impl Automation {
    pub(crate) fn ai_summarize(&mut self, a: &Args) -> Result<Value> {
        let doc = self.doc_to_copy(a)?;
        let page = |key| usize::try_from(a.int(key)?).map_err(|_| ToolError::InvalidArgs("Page numbers must be positive.".into()));
        let (first, last) = (page("first_page")?, page("last_page")?);
        let source = SummarySource::new(doc, first, last).map_err(failed)?;
        let providers = ai::load().map_err(failed)?;
        let profile = providers.enabled_profile(a.opt_str("provider")?).map_err(failed)?;
        let endpoint = profile.endpoint().map_err(failed)?;
        let mut result = json!({
            "provider": profile.name, "endpoint": endpoint.as_str(), "model": profile.model,
            "first_page": first, "last_page": last, "sent": false
        });
        if !a.opt_bool("user_confirmed")?.unwrap_or(false) {
            return Ok(result);
        }
        if a.opt_str("confirmed_endpoint")? != Some(endpoint.as_str()) || a.opt_str("confirmed_model")? != Some(profile.model.as_str()) {
            return Err(failed("The endpoint and model must match the preview. Preview again and confirm the destination before sending."));
        }
        let text = source.extract().map_err(failed)?;
        let summary = ai::summarize(profile, &text).map_err(failed)?;
        result["sent"] = json!(true);
        result["summary"] = json!(summary);
        Ok(result)
    }
}
