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

/// Validates a plugin's placements against the request it answered.
///
/// A plugin result is accepted only when:
/// - every requested `WindowId` appears exactly once (no duplicates, no
///   missing, no foreign windows),
/// - the placement count equals the requested window count,
/// - all coordinates are finite,
/// - width and height are strictly positive,
/// - frames stay within a reasonable bound around the requested area
///   (2x area extent in each direction; catches runaway geometry).
///
/// On any violation the caller must discard ALL plugin output and fall back
/// to the built-in layout — never partially apply invalid output.
pub fn validate_placements(
    request: &PluginRequest,
    placements: &[PluginPlacement],
) -> Result<(), String> {
    if placements.len() != request.windows.len() {
        return Err(format!(
            "placement count {} != requested window count {}",
            placements.len(),
            request.windows.len()
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for p in placements {
        if !request.windows.contains(&p.window) {
            return Err(format!("foreign window {:?} in placements", p.window));
        }
        if !seen.insert(p.window) {
            return Err(format!("duplicate window {:?} in placements", p.window));
        }
        let f = p.frame;
        if !(f.x.is_finite() && f.y.is_finite() && f.width.is_finite() && f.height.is_finite()) {
            return Err(format!("non-finite geometry for window {:?}", p.window));
        }
        if f.width <= 0.0 || f.height <= 0.0 {
            return Err(format!(
                "non-positive size for window {:?}: {}x{}",
                p.window, f.width, f.height
            ));
        }
        // Reasonable bounds: allow up to 2x the requested area extent around
        // the area origin (covers negative-coordinate secondary displays via
        // the area origin offset while catching absurd values).
        let max_w = request.area.width * 2.0 + 1.0;
        let max_h = request.area.height * 2.0 + 1.0;
        let min_x = request.area.x - max_w;
        let max_x = request.area.x + request.area.width + max_w;
        let min_y = request.area.y - max_h;
        let max_y = request.area.y + request.area.height + max_h;
        if f.width > max_w
            || f.height > max_h
            || f.x < min_x
            || f.y < min_y
            || f.x + f.width > max_x
            || f.y + f.height > max_y
        {
            return Err(format!(
                "frame for window {:?} exceeds bounds around area {:?}: {:?}",
                p.window, request.area, f
            ));
        }
    }
    Ok(())
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

pub mod wasm_runtime {
    use super::LayoutPlugin;
    use super::{wasm_abi, PluginError, PluginManifest, PluginPlacement, PluginRequest};
    use std::path::Path;
    use wasmi::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

    /// Hard cap on plugin linear memory (16 MiB). Enforced by wasmi's
    /// `StoreLimits` resource limiter attached to the per-call `Store`.
    pub const PLUGIN_MEMORY_LIMIT_BYTES: usize = 16 * 1024 * 1024;
    /// Hard cap on table growth per plugin instance.
    pub const PLUGIN_TABLE_MAX_ELEMENTS: u32 = 10_000;
    /// Fuel budget (~1M instructions) acting as the compute timeout.
    const PLUGIN_FUEL: u64 = 1_000_000;

    pub struct WasmPlugin {
        engine: Engine,
        module: Module,
        manifest: PluginManifest,
    }

    impl WasmPlugin {
        pub fn load_bytes(bytes: &[u8], manifest: PluginManifest) -> Result<Self, PluginError> {
            if manifest.name.is_empty() {
                return Err(PluginError::InvalidRequest("manifest name empty".into()));
            }
            // Validate ABI version if present in manifest extra
            let mut config = Config::default();
            config.consume_fuel(true);
            let engine = Engine::new(&config);
            let module =
                Module::new(&engine, bytes).map_err(|e| PluginError::Execution(e.to_string()))?;
            // Basic export validation: must have memory, alloc, compute
            Ok(Self {
                engine,
                module,
                manifest,
            })
        }

        pub fn load_file(path: &Path) -> Result<Self, PluginError> {
            let bytes = std::fs::read(path).map_err(|e| PluginError::Execution(e.to_string()))?;
            let manifest_path = path.with_extension("json");
            let manifest = if manifest_path.exists() {
                let data = std::fs::read_to_string(&manifest_path)
                    .map_err(|e| PluginError::Execution(e.to_string()))?;
                let v: serde_json::Value = serde_json::from_str(&data)
                    .map_err(|e| PluginError::Execution(e.to_string()))?;
                let name = v
                    .get("name")
                    .and_then(|s| s.as_str())
                    .unwrap_or("wasm_plugin")
                    .to_string();
                let version = v
                    .get("version")
                    .and_then(|s| s.as_str())
                    .unwrap_or("0.1.0")
                    .to_string();
                let abi = v
                    .get("abi_version")
                    .and_then(|n| n.as_u64())
                    .unwrap_or(crate::wasm_abi::ABI_VERSION as u64);
                if abi != crate::wasm_abi::ABI_VERSION as u64 {
                    return Err(PluginError::Execution(format!(
                        "abi_version mismatch expected {} got {}",
                        crate::wasm_abi::ABI_VERSION,
                        abi
                    )));
                }
                PluginManifest {
                    name,
                    version,
                    description: v
                        .get("description")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string()),
                }
            } else {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("wasm_plugin")
                    .to_string();
                PluginManifest {
                    name,
                    version: "0.1.0".into(),
                    description: None,
                }
            };
            Self::load_bytes(&bytes, manifest)
        }

        fn instantiate(
            &self,
            store: &mut Store<StoreLimits>,
        ) -> Result<wasmi::Instance, PluginError> {
            let linker = Linker::new(&self.engine);
            let pre = linker
                .instantiate(&mut *store, &self.module)
                .map_err(|e| PluginError::Execution(e.to_string()))?;
            pre.start(&mut *store)
                .map_err(|e| PluginError::Execution(e.to_string()))
        }
    }

    impl WasmPlugin {
        /// Creates a fuel- and resource-bounded store for one plugin call.
        fn make_store(&self) -> Store<StoreLimits> {
            let limits = StoreLimitsBuilder::new()
                .memory_size(PLUGIN_MEMORY_LIMIT_BYTES)
                .table_elements(PLUGIN_TABLE_MAX_ELEMENTS)
                .memories(1)
                .tables(1)
                .instances(1)
                .trap_on_grow_failure(true)
                .build();
            let mut store = Store::new(&self.engine, limits);
            store.limiter(|limits: &mut StoreLimits| limits);
            store
        }
    }

    impl LayoutPlugin for WasmPlugin {
        fn manifest(&self) -> PluginManifest {
            self.manifest.clone()
        }
        fn compute(&self, request: &PluginRequest) -> Result<Vec<PluginPlacement>, PluginError> {
            let input = wasm_abi::encode_request(request);
            // Bounded store: fuel budget acts as timeout; StoreLimits caps
            // linear memory (16 MiB) and table growth. No host imports are
            // linked, so a malicious plugin can only trap inside its sandbox.
            let mut store = self.make_store();
            store
                .set_fuel(PLUGIN_FUEL)
                .map_err(|e| PluginError::Execution(e.to_string()))?;
            let instance = self.instantiate(&mut store)?;
            // Get memory and alloc/compute exports
            let memory = instance
                .get_memory(&store, "memory")
                .ok_or_else(|| PluginError::Execution("missing memory export".into()))?;
            let alloc: wasmi::TypedFunc<(i32,), i32> = instance
                .get_typed_func(&store, "alloc")
                .map_err(|_| PluginError::Execution("missing alloc export".into()))?;
            let compute: wasmi::TypedFunc<(i32, i32), i64> =
                instance.get_typed_func(&store, "compute").map_err(|_| {
                    PluginError::Execution("missing compute export (i32,i32)->i64".into())
                })?;
            // Allocate input in wasm memory
            let input_len = input.len() as i32;
            let input_ptr = alloc
                .call(&mut store, (input_len,))
                .map_err(|e| PluginError::Execution(format!("alloc trap: {e}")))?;
            memory
                .write(&mut store, input_ptr as usize, &input)
                .map_err(|e| PluginError::Execution(e.to_string()))?;
            // Call compute, packed output ptr/len as i64
            let packed = compute
                .call(&mut store, (input_ptr, input_len))
                .map_err(|e| PluginError::Execution(format!("compute trap: {e}")))?;
            let out_ptr = (packed & 0xFFFF_FFFF) as i32;
            let out_len = ((packed >> 32) & 0xFFFF_FFFF) as i32;
            if out_ptr < 0 || !(0..=1024 * 1024).contains(&out_len) {
                return Err(PluginError::Execution(format!(
                    "invalid output ptr/len {out_ptr}/{out_len}"
                )));
            }
            let mut output = vec![0u8; out_len as usize];
            memory
                .read(&store, out_ptr as usize, &mut output[..])
                .map_err(|e| PluginError::Execution(e.to_string()))?;
            // Optional dealloc: if plugin exports dealloc, free input/output
            if let Ok(dealloc) = instance.get_typed_func::<(i32, i32), ()>(&store, "dealloc") {
                let _ = dealloc.call(&mut store, (input_ptr, input_len));
                let _ = dealloc.call(&mut store, (out_ptr, out_len));
            }
            // Check fuel exhausted -> timeout
            if store.get_fuel().map(|f| f == 0).unwrap_or(false) {
                return Err(PluginError::Execution("fuel exhausted (timeout)".into()));
            }
            wasm_abi::decode_response(&output).map_err(|e| PluginError::Execution(e.to_string()))
        }
    }

    impl super::Registry {
        pub fn load_wasm_file(&mut self, path: &Path) -> Result<String, PluginError> {
            let plugin = WasmPlugin::load_file(path)?;
            let name = plugin.manifest().name.clone();
            if self.get(&name).is_some() {
                return Err(PluginError::Execution(format!(
                    "plugin {} already registered",
                    name
                )));
            }
            self.register(plugin);
            Ok(name)
        }
        pub fn load_wasm_bytes(
            &mut self,
            bytes: &[u8],
            manifest: PluginManifest,
        ) -> Result<String, PluginError> {
            let plugin = WasmPlugin::load_bytes(bytes, manifest.clone())?;
            let name = manifest.name.clone();
            if self.get(&name).is_some() {
                return Err(PluginError::Execution(format!(
                    "plugin {} already registered",
                    name
                )));
            }
            self.register(plugin);
            Ok(name)
        }
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

    #[test]
    fn wasm_plugin_load_and_compute() {
        // Minimal WASM plugin that returns "[]" for any input, using alloc/compute ABI
        let wat = r#"
            (module
                (memory (export "memory") 1)
                (global $heap (mut i32) (i32.const 1024))
                (func $alloc (export "alloc") (param $size i32) (result i32)
                    (local $ptr i32)
                    (global.get $heap)
                    (local.set $ptr)
                    (global.set $heap (i32.add (global.get $heap) (local.get $size)))
                    (local.get $ptr)
                )
                (func (export "compute") (param $in_ptr i32) (param $in_len i32) (result i64)
                    (local $out_ptr i32)
                    (local.set $out_ptr (call $alloc (i32.const 2)))
                    (i32.store8 (local.get $out_ptr) (i32.const 91)) ;; '['
                    (i32.store8 (i32.add (local.get $out_ptr) (i32.const 1)) (i32.const 93)) ;; ']'
                    (i64.or
                        (i64.extend_i32_u (local.get $out_ptr))
                        (i64.shl (i64.extend_i32_u (i32.const 2)) (i64.const 32))
                    )
                )
            )
        "#;
        let wasm = wat::parse_str(wat).unwrap();
        let manifest = PluginManifest {
            name: "test_wasm".into(),
            version: "0.1.0".into(),
            description: None,
        };
        let mut reg = Registry::new();
        reg.load_wasm_bytes(&wasm, manifest).unwrap();
        let plugin = reg.get("test_wasm").unwrap();
        let req = PluginRequest {
            area: Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            windows: vec![WindowId(1)],
            gap: 8.0,
            padding: 8.0,
        };
        let placements = plugin.compute(&req).unwrap();
        assert_eq!(
            placements.len(),
            0,
            "empty wasm should return empty placements and be isolated"
        );
    }

    #[test]
    fn wasm_plugin_timeout_isolated() {
        // Infinite loop WASM should hit fuel limit and not crash host
        let wat = r#"
            (module
                (memory (export "memory") 1)
                (func (export "alloc") (param $size i32) (result i32) (i32.const 0))
                (func (export "compute") (param $in_ptr i32) (param $in_len i32) (result i64)
                    (loop $l (br $l))
                    (i64.const 0)
                )
            )
        "#;
        let wasm = wat::parse_str(wat).unwrap();
        let manifest = PluginManifest {
            name: "loop".into(),
            version: "0.1.0".into(),
            description: None,
        };
        let mut reg = Registry::new();
        reg.load_wasm_bytes(&wasm, manifest).unwrap();
        let plugin = reg.get("loop").unwrap();
        let req = PluginRequest {
            area: Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            windows: vec![],
            gap: 0.0,
            padding: 0.0,
        };
        let err = plugin.compute(&req).unwrap_err();
        assert!(
            err.to_string().contains("fuel")
                || err.to_string().contains("trap")
                || err.to_string().contains("timeout"),
            "should be fuel timeout, got {err}"
        );
    }

    // ---- Blocker 12: plugin output validation ----

    fn req_for(windows: &[u32]) -> PluginRequest {
        PluginRequest {
            area: Rect {
                x: 0.0,
                y: 0.0,
                width: 1000.0,
                height: 800.0,
            },
            windows: windows.iter().map(|w| WindowId(*w)).collect(),
            gap: 0.0,
            padding: 0.0,
        }
    }

    fn place(window: u32, frame: Rect) -> PluginPlacement {
        PluginPlacement {
            window: WindowId(window),
            frame,
        }
    }

    #[test]
    fn blocker12_valid_response_accepted() {
        let req = req_for(&[1, 2]);
        let placements = vec![
            place(
                1,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 500.0,
                    height: 800.0,
                },
            ),
            place(
                2,
                Rect {
                    x: 500.0,
                    y: 0.0,
                    width: 500.0,
                    height: 800.0,
                },
            ),
        ];
        assert!(validate_placements(&req, &placements).is_ok());
    }

    #[test]
    fn blocker12_empty_response_rejected() {
        let req = req_for(&[1, 2]);
        assert!(
            validate_placements(&req, &[]).is_err(),
            "empty response for non-empty request must be rejected"
        );
    }

    #[test]
    fn blocker12_duplicate_window_rejected() {
        let req = req_for(&[1, 2]);
        let placements = vec![
            place(
                1,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 500.0,
                    height: 800.0,
                },
            ),
            place(
                1,
                Rect {
                    x: 500.0,
                    y: 0.0,
                    width: 500.0,
                    height: 800.0,
                },
            ),
        ];
        assert!(validate_placements(&req, &placements).is_err());
    }

    #[test]
    fn blocker12_missing_window_rejected() {
        let req = req_for(&[1, 2]);
        let placements = vec![place(
            1,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 1000.0,
                height: 800.0,
            },
        )];
        assert!(validate_placements(&req, &placements).is_err());
    }

    #[test]
    fn blocker12_foreign_window_rejected() {
        let req = req_for(&[1]);
        let placements = vec![
            place(
                1,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 500.0,
                    height: 800.0,
                },
            ),
            place(
                99,
                Rect {
                    x: 500.0,
                    y: 0.0,
                    width: 500.0,
                    height: 800.0,
                },
            ),
        ];
        assert!(validate_placements(&req, &placements).is_err());
    }

    #[test]
    fn blocker12_nan_geometry_rejected() {
        let req = req_for(&[1]);
        let placements = vec![place(
            1,
            Rect {
                x: f64::NAN,
                y: 0.0,
                width: 500.0,
                height: 800.0,
            },
        )];
        assert!(validate_placements(&req, &placements).is_err());
    }

    #[test]
    fn blocker12_infinite_geometry_rejected() {
        let req = req_for(&[1]);
        let placements = vec![place(
            1,
            Rect {
                x: 0.0,
                y: 0.0,
                width: f64::INFINITY,
                height: 800.0,
            },
        )];
        assert!(validate_placements(&req, &placements).is_err());
    }

    #[test]
    fn blocker12_zero_and_negative_sizes_rejected() {
        let req = req_for(&[1]);
        let zero = vec![place(
            1,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 800.0,
            },
        )];
        assert!(validate_placements(&req, &zero).is_err());
        let negative = vec![place(
            1,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 500.0,
                height: -3.0,
            },
        )];
        assert!(validate_placements(&req, &negative).is_err());
    }

    #[test]
    fn blocker12_absurd_frame_bounds_rejected() {
        let req = req_for(&[1]);
        let huge = vec![place(
            1,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 1.0e9,
                height: 800.0,
            },
        )];
        assert!(validate_placements(&req, &huge).is_err());
    }

    #[test]
    fn blocker12_huge_origins_rejected_on_negative_coordinate_displays() {
        let mut req = req_for(&[1]);
        req.area.x = -1920.0;
        let positive = vec![place(
            1,
            Rect {
                x: 1.0e12,
                y: 0.0,
                width: 500.0,
                height: 800.0,
            },
        )];
        assert!(validate_placements(&req, &positive).is_err());
        let negative = vec![place(
            1,
            Rect {
                x: -1.0e12,
                y: 0.0,
                width: 500.0,
                height: 800.0,
            },
        )];
        assert!(validate_placements(&req, &negative).is_err());
    }

    // ---- Blocker 11: WASM memory limiting ----

    #[test]
    fn blocker11_memory_growth_past_limit_is_contained() {
        // Plugin that repeatedly grows linear memory by 16 pages (1 MiB) per
        // iteration until growth fails or fuel runs out. With the 16 MiB
        // StoreLimits attached, growth beyond the cap must trap/fail instead
        // of consuming unbounded host memory.
        let wat = r#"
            (module
                (memory (export "memory") 1)
                (func (export "alloc") (param $size i32) (result i32) (i32.const 0))
                (func (export "compute") (param $in_ptr i32) (param $in_len i32) (result i64)
                    (loop $l
                        (drop (memory.grow (i32.const 16)))
                        (br $l)
                    )
                    (i64.const 0)
                )
            )
        "#;
        let wasm = wat::parse_str(wat).unwrap();
        let manifest = PluginManifest {
            name: "hogger".into(),
            version: "0.1.0".into(),
            description: None,
        };
        let mut reg = Registry::new();
        reg.load_wasm_bytes(&wasm, manifest).unwrap();
        let plugin = reg.get("hogger").unwrap();
        let req = req_for(&[]);
        let err = plugin
            .compute(&req)
            .expect_err("memory-hogging plugin must not succeed");
        let msg = err.to_string();
        assert!(
            msg.contains("trap") || msg.contains("memory") || msg.contains("fuel"),
            "expected containment error, got: {msg}"
        );
        // The host registry is still alive and usable afterwards.
        assert!(reg.get("hogger").is_some());
    }
}
