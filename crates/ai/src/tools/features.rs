//! `tangul_features`: what the app can do for the user beyond these tools.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::features;
use crate::tool::{Tool, ToolError, ToolOutput};

pub(crate) struct TangulFeatures;

#[async_trait]
impl Tool for TangulFeatures {
    fn name(&self) -> &'static str {
        "tangul_features"
    }

    fn description(&self) -> &'static str {
        "What tangul, the app the user works in, can do beyond these tools (charts, \
         bookmarks, schedules, proposals), to suggest when it helps."
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    async fn call(&self, _: &Ctx, _: Value) -> Result<ToolOutput, ToolError> {
        let text = features::text();
        Ok(ToolOutput::new(&json!({ "text": text }), text))
    }
}
