use rovr_types::{Rect, WindowId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRequest {
    pub area: Rect,
    pub windows: Vec<WindowId>,
    pub gap: f64,
    pub padding: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginPlacement {
    pub window: WindowId,
    pub frame: Rect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
}

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("plugin error: {0}")]
    Execution(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("plugin not found: {0}")]
    NotFound(String),
}

pub trait LayoutPlugin: Send + Sync {
    fn manifest(&self) -> PluginManifest;
    fn compute(&self, request: &PluginRequest) -> Result<Vec<PluginPlacement>, PluginError>;
}

#[derive(Default)]
pub struct Registry {
    plugins: Vec<Box<dyn LayoutPlugin>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, plugin: impl LayoutPlugin + 'static) {
        self.plugins.push(Box::new(plugin));
    }

    pub fn get(&self, name: &str) -> Option<&dyn LayoutPlugin> {
        self.plugins
            .iter()
            .find(|p| p.manifest().name == name)
            .map(|p| p.as_ref() as &dyn LayoutPlugin)
    }

    pub fn names(&self) -> Vec<String> {
        self.plugins
            .iter()
            .map(|p| p.manifest().name.clone())
            .collect()
    }
}

pub mod wasm_abi {
    use super::{PluginPlacement, PluginRequest};

    pub const ABI_VERSION: u32 = 1;

    pub fn encode_request(req: &PluginRequest) -> Vec<u8> {
        serde_json::to_vec(req).unwrap_or_default()
    }

    pub fn decode_request(bytes: &[u8]) -> Result<PluginRequest, String> {
        serde_json::from_slice(bytes).map_err(|e| e.to_string())
    }

    pub fn encode_response(placements: &[PluginPlacement]) -> Vec<u8> {
        serde_json::to_vec(placements).unwrap_or_default()
    }

    pub fn decode_response(bytes: &[u8]) -> Result<Vec<PluginPlacement>, String> {
        serde_json::from_slice(bytes).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rovr_types::{Rect, WindowId};

    struct EchoPlugin;

    impl LayoutPlugin for EchoPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                name: "echo".into(),
                version: "0.1.0".into(),
                description: None,
            }
        }

        fn compute(&self, req: &PluginRequest) -> Result<Vec<PluginPlacement>, PluginError> {
            if req.windows.is_empty() {
                return Ok(vec![]);
            }
            Ok(req
                .windows
                .iter()
                .map(|w| PluginPlacement {
                    window: *w,
                    frame: req.area,
                })
                .collect())
        }
    }

    #[test]
    fn registry_round_trips() {
        let mut reg = Registry::new();
        reg.register(EchoPlugin);
        assert_eq!(reg.names(), vec!["echo"]);
        let p = reg.get("echo").unwrap();
        assert_eq!(p.manifest().version, "0.1.0");
        let req = PluginRequest {
            area: Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            windows: vec![WindowId(1), WindowId(2)],
            gap: 8.0,
            padding: 8.0,
        };
        let placements = p.compute(&req).unwrap();
        assert_eq!(placements.len(), 2);
    }

    #[test]
    fn wasm_abi_json_round_trips() {
        let req = PluginRequest {
            area: Rect {
                x: 10.0,
                y: 10.0,
                width: 800.0,
                height: 600.0,
            },
            windows: vec![WindowId(5)],
            gap: 4.0,
            padding: 4.0,
        };
        let bytes = wasm_abi::encode_request(&req);
        let back = wasm_abi::decode_request(&bytes).unwrap();
        assert_eq!(back.windows, req.windows);
        assert_eq!(back.area.x, 10.0);
    }
}
