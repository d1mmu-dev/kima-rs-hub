//! Execution provider selection for the bounded ONNX session pools.
//!
//! CPU is the default and the reference. CUDA is opt-in: it needs a CUDA-enabled ONNX
//! Runtime build at `ORT_DYLIB_PATH` (the official `Microsoft.ML.OnnxRuntime.Gpu.Linux` 1.22
//! package ships `libonnxruntime.so` with `libonnxruntime_providers_{shared,cuda}.so` next to
//! it) plus the CUDA 12 / cuDNN 9 user-space libraries, either on the loader path or in
//! [`ProviderConfig::cuda_lib_dir`]. If any CUDA step fails, the pool is built on the CPU
//! instead and [`ActiveProvider::CpuFallback`] records why; nothing is downloaded or searched
//! for.
//!
//! Admission: a CPU session reserves four times its encoded size against the process host
//! budget (weights, prepacked copies and activations live in host memory). A CUDA session
//! reserves its encoded size once (the verified bytes and ORT's transient host copy while
//! initializers are uploaded); its weights and activations live in device memory, which is
//! bounded separately per session by [`ProviderConfig::cuda_mem_limit_bytes`] (the CUDA EP
//! `gpu_mem_limit`), not by the host budget.
use std::path::PathBuf;

/// Requested execution provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionProvider {
    #[default]
    Cpu,
    Cuda,
}

impl std::str::FromStr for ExecutionProvider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cpu" => Ok(Self::Cpu),
            "cuda" => Ok(Self::Cuda),
            other => Err(format!(
                "unknown ONNX execution provider {other:?} (cpu|cuda)"
            )),
        }
    }
}

/// Provider request for one pool.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderConfig {
    pub provider: ExecutionProvider,
    /// CUDA device ordinal.
    pub cuda_device_id: i32,
    /// Directory holding `libcudart.so.12`, `libcublas(Lt).so.12`, `libcufft.so.11`,
    /// `libcurand.so.10`, `libnvrtc.so.12` and `libcudnn*.so.9`; preloaded once by absolute
    /// path. `None` leaves resolution to the dynamic loader.
    pub cuda_lib_dir: Option<PathBuf>,
    /// Per-session CUDA arena limit in bytes; `None` is ORT's default (unbounded).
    pub cuda_mem_limit_bytes: Option<usize>,
}

impl ProviderConfig {
    pub fn cpu() -> Self {
        Self::default()
    }
}

/// Provider a pool actually runs on.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum ActiveProvider {
    Cpu,
    Cuda {
        device_id: i32,
    },
    /// CUDA was requested but could not be used; the pool runs on the CPU.
    CpuFallback {
        reason: String,
    },
}

impl ActiveProvider {
    pub fn is_cuda(&self) -> bool {
        matches!(self, Self::Cuda { .. })
    }
}

/// Preloads the CUDA/cuDNN libraries from `dir` once per process; later calls return the
/// first result (a different directory is not retried).
pub(super) fn preload_cuda(dir: &std::path::Path) -> Result<(), String> {
    static PRELOAD: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    PRELOAD
        .get_or_init(|| {
            if !dir.is_absolute() || !dir.is_dir() {
                return Err(format!(
                    "CUDA library directory {} must be an existing absolute directory",
                    dir.display()
                ));
            }
            std::panic::catch_unwind(|| {
                ort::execution_providers::cuda::preload_dylibs(Some(dir), Some(dir))
            })
            .map_err(|_| "preloading CUDA libraries panicked".to_string())?
            .map_err(|e| format!("preloading CUDA libraries from {}: {e}", dir.display()))
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_provider_names() {
        assert_eq!(
            "cpu".parse::<ExecutionProvider>(),
            Ok(ExecutionProvider::Cpu)
        );
        assert_eq!(
            " CUDA ".parse::<ExecutionProvider>(),
            Ok(ExecutionProvider::Cuda)
        );
        assert!("rocm".parse::<ExecutionProvider>().is_err());
    }

    #[test]
    fn active_provider_serializes_with_tag() {
        let json = serde_json::to_string(&ActiveProvider::Cuda { device_id: 0 }).unwrap();
        assert_eq!(json, r#"{"provider":"cuda","device_id":0}"#);
        let json =
            serde_json::to_string(&ActiveProvider::CpuFallback { reason: "x".into() }).unwrap();
        assert_eq!(json, r#"{"provider":"cpu_fallback","reason":"x"}"#);
    }
}
