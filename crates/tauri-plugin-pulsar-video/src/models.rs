use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachRequest {
  /// Solid color to paint the test surface (hex, e.g. "#00E5FF"). M2 spike only —
  /// later this is replaced by the decoder's output surface.
  pub color: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachResponse {
  pub ok: bool,
  // Some Android @Command handlers (e.g. setStatusBar) resolve with just `{ok}`,
  // so accept a missing `detail` instead of failing deserialization.
  #[serde(default)]
  pub detail: String,
}
