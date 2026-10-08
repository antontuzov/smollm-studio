//! Hardware, server, benchmark, log and app-info DTOs.

use serde::{Deserialize, Serialize};

/// Compute backend an engine should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    #[default]
    Cpu,
    Metal,
    Cuda,
    Vulkan,
    /// MockEngine is always available and reports itself as `mock`.
    Mock,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Metal => "metal",
            Self::Cuda => "cuda",
            Self::Vulkan => "vulkan",
            Self::Mock => "mock",
        }
    }

    pub fn is_accelerated(self) -> bool {
        !matches!(self, Self::Cpu | Self::Mock)
    }

    /// Name a llama.cpp compute backend, so an engine can report the device it
    /// actually got instead of the one the settings asked for.
    ///
    /// llama.cpp registers its Metal backend as `MTL`, not `Metal`. It also
    /// builds OpenCL, SYCL and vendor-specific backends this app does not
    /// target; an unrecognised name is reported as CPU, and the engine logs the
    /// name it saw rather than inventing a match.
    #[must_use]
    pub fn from_ggml_backend(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "metal" | "mtl" => Self::Metal,
            "cuda" => Self::Cuda,
            "vulkan" => Self::Vulkan,
            _ => Self::Cpu,
        }
    }
}

/// Operating system label used in the doctor report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Macos,
    Windows,
    Linux,
    #[default]
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Macos => "macOS",
            Self::Windows => "Windows",
            Self::Linux => "Linux",
            Self::Other => "unknown",
        }
    }

    /// Machine-readable id, matching the serde representation.
    pub fn id(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Other => "other",
        }
    }
}

/// An accelerator the inference library itself says it can use.
///
/// Reported by llama.cpp rather than guessed from the platform: it is the
/// memory the library is *willing* to hand to the device, which on Apple
/// Silicon is a capped slice of unified RAM, not the whole of it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Accelerator {
    /// Device name inside the library, e.g. `MTL0`.
    pub name: String,
    /// Human readable device, e.g. `Apple M1`.
    pub description: String,
    /// Backend the device belongs to, e.g. `MTL`, `CUDA`, `Vulkan`.
    pub backend: String,
    /// GiB the library will let this device hold weights in.
    pub usable_memory_gb: f64,
    /// GiB of that budget still free right now.
    pub free_memory_gb: f64,
}

/// Everything we could measure about the machine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HardwareReport {
    pub platform: Platform,
    pub arch: String,
    pub cpu_brand: String,
    pub logical_cores: usize,
    pub physical_cores: usize,
    pub total_ram_gb: f64,
    pub available_ram_gb: f64,
    pub apple_silicon: bool,
    pub metal_available: bool,
    pub nvidia_gpu: Option<String>,
    pub vulkan_available: bool,
    /// Best-effort human readable GPU name.
    pub gpu_name: String,
    pub disk_free_gb: f64,
    /// Free space on the volume holding the model directory.
    pub model_volume_free_gb: f64,
    /// What the linked engine can offload to, `None` until the engine has been
    /// asked (a build without a native engine never can).
    pub accelerator: Option<Accelerator>,
}

/// Actionable summary the Home page renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    pub platform: String,
    pub arch: String,
    pub cpu_cores: usize,
    pub ram_gb: f64,
    pub gpu: String,
    pub backend_recommendation: Backend,
    pub recommended_models: Vec<String>,
    pub warnings: Vec<String>,
    /// Headline sentence, e.g. "Your machine looks great for 0.5B-3B models."
    pub headline: String,
    /// Supporting detail, e.g. quantisation advice for low-RAM machines.
    pub detail: String,
    pub max_comfortable_parameters_b: f32,
    pub hardware: HardwareReport,
}

/// Local OpenAI-compatible server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    /// Model served when a request omits `model` or asks for `default`.
    pub default_model_id: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            // Loopback only: the server never binds to a routable interface.
            host: "127.0.0.1".to_string(),
            port: 8080,
            default_model_id: None,
        }
    }
}

impl ServerConfig {
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    pub fn validate(&self) -> Result<(), crate::AppError> {
        if self.port == 0 {
            return Err(crate::AppError::InvalidRequest(
                "port must be 1-65535".into(),
            ));
        }
        // Anything else (including 0.0.0.0) would expose the server to the
        // network; SmolLLM Studio keeps generated completions local.
        let loopback = matches!(self.host.trim(), "127.0.0.1" | "localhost" | "::1");
        if !loopback || self.host.trim().is_empty() {
            return Err(crate::AppError::InvalidRequest(format!(
                "host must be a loopback address such as 127.0.0.1, got {}",
                self.host
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub running: bool,
    pub host: String,
    pub port: u16,
    pub base_url: String,
    pub engine: String,
    pub loaded_model: Option<String>,
    /// Model ids `/v1/models` offers, resident one first. Empty while the server
    /// is stopped, where it means "what the library could serve".
    pub served_models: Vec<String>,
    pub requests: u64,
    pub uptime_seconds: u64,
    pub simulated: bool,
}

impl Default for ServerStatus {
    fn default() -> Self {
        Self {
            running: false,
            host: ServerConfig::default().host,
            port: ServerConfig::default().port,
            base_url: String::new(),
            engine: String::new(),
            loaded_model: None,
            served_models: Vec::new(),
            requests: 0,
            uptime_seconds: 0,
            simulated: false,
        }
    }
}

/// Which subsystem produced a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    App,
    Engine,
    Download,
    Server,
}

impl LogStream {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Engine => "engine",
            Self::Download => "download",
            Self::Server => "server",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub timestamp_ms: u64,
    pub level: String,
    pub target: String,
    pub stream: LogStream,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LogFilter {
    pub levels: Vec<String>,
    pub stream: Option<LogStream>,
    pub contains: Option<String>,
    pub limit: Option<usize>,
}

/// Metadata shown in Settings and used by the onboarding screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub tagline: String,
    pub engine: String,
    pub simulated_engine: bool,
    pub platform: String,
    pub arch: String,
    pub data_dir: String,
    pub models_dir: String,
    pub logs_dir: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_defaults_to_loopback() {
        let config = ServerConfig::default();
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.base_url(), "http://127.0.0.1:8080");
        config
            .validate()
            .unwrap_or_else(|err| panic!("default config should validate: {err}"));
    }

    #[test]
    fn server_refuses_public_interfaces() {
        // Binding a routable address would expose local inference to the network.
        for host in ["0.0.0.0", "192.168.1.20", "::", "example.com", ""] {
            let config = ServerConfig {
                host: host.to_string(),
                ..ServerConfig::default()
            };
            assert!(config.validate().is_err(), "host {host:?} must be rejected");
        }
        for host in ["127.0.0.1", "localhost", "::1", " 127.0.0.1 "] {
            let config = ServerConfig {
                host: host.to_string(),
                ..ServerConfig::default()
            };
            assert!(config.validate().is_ok(), "host {host:?} should be allowed");
        }
    }

    #[test]
    fn backend_maps_llama_cpp_names_onto_supported_devices() {
        // llama.cpp spells Metal `MTL` in its own device registry.
        assert_eq!(Backend::from_ggml_backend("MTL"), Backend::Metal);
        assert_eq!(Backend::from_ggml_backend("Metal"), Backend::Metal);
        assert_eq!(Backend::from_ggml_backend("CUDA"), Backend::Cuda);
        assert_eq!(Backend::from_ggml_backend(" vulkan "), Backend::Vulkan);
        assert_eq!(Backend::from_ggml_backend("CPU"), Backend::Cpu);
        // Not a target this app builds for: named in the log, never guessed at.
        assert_eq!(Backend::from_ggml_backend("OpenCL"), Backend::Cpu);
    }

    #[test]
    fn accelerator_reports_the_names_the_frontend_reads() {
        let json = serde_json::to_value(Accelerator {
            name: "MTL0".to_string(),
            description: "Apple M1".to_string(),
            backend: "MTL".to_string(),
            usable_memory_gb: 11.8,
            free_memory_gb: 11.7,
        })
        .expect("serialises");
        assert_eq!(json["description"], "Apple M1");
        assert_eq!(json["usableMemoryGb"], 11.8);
        assert_eq!(json["freeMemoryGb"], 11.7);
    }

    #[test]
    fn server_rejects_port_zero() {
        let config = ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            default_model_id: None,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn backend_acceleration_flags() {
        assert!(!Backend::Cpu.is_accelerated());
        assert!(!Backend::Mock.is_accelerated());
        assert!(Backend::Metal.is_accelerated());
        assert!(Backend::Cuda.is_accelerated());
        assert!(Backend::Vulkan.is_accelerated());
        assert_eq!(Backend::Metal.as_str(), "metal");
    }

    #[test]
    fn log_stream_names_are_stable() {
        assert_eq!(LogStream::App.as_str(), "app");
        assert_eq!(LogStream::Engine.as_str(), "engine");
        assert_eq!(LogStream::Download.as_str(), "download");
        assert_eq!(LogStream::Server.as_str(), "server");
    }

    #[test]
    fn platform_labels_render_for_ui() {
        assert_eq!(Platform::Macos.label(), "macOS");
        assert_eq!(Platform::Windows.label(), "Windows");
        assert_eq!(Platform::Linux.label(), "Linux");
    }

    #[test]
    fn platform_ids_match_json() {
        assert_eq!(Platform::Macos.id(), "macos");
        assert_eq!(Platform::Windows.id(), "windows");
        assert_eq!(Platform::Linux.id(), "linux");
        assert_eq!(Platform::Other.id(), "other");
        assert_eq!(
            serde_json::to_string(&Platform::Macos).unwrap_or_default(),
            "\"macos\""
        );
    }
}
