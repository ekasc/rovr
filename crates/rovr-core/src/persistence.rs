use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::layout_state::LayoutState;
use crate::workspace::WorkspaceState;

/// Serialized form of the engine's mutable runtime state.
///
/// `layouts` is keyed by `SpaceId` rendered as a string: JSON object keys must
/// be strings, so we never serialize `HashMap<SpaceId, _>` directly (that
/// would hit serde_json's non-string-key restriction). `scratchpads` is keyed
/// by scratchpad name (already a string).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PersistedState {
    pub layouts: HashMap<String, LayoutState>,
    pub scratchpads: HashMap<String, bool>,
    #[serde(default)]
    pub workspaces: HashMap<String, WorkspaceState>,
}
