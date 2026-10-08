//! Mapping from a requested compute backend to an engine this build can run.
//!
//! SmolLLM Studio ships with MockEngine enabled everywhere. llama.cpp and Candle
//! are cargo features (and, for llama.cpp, a native toolchain), so a user who
//! picks Metal on a build without those bindings must be told the truth rather
//! than quietly shown simulated numbers. The features compile adapter code but
//! link no library, so `is_available` — not `compiled` — is what decides here.

use smollm_core::system::{Backend, HardwareReport};
use smollm_engine::EngineKind;

/// Resolve which engine can serve `backend` in this binary.
pub fn kind_for_backend(backend: Backend) -> EngineKind {
    if backend == Backend::Mock {
        return EngineKind::Mock;
    }
    if EngineKind::LlamaCpp.is_available() {
        return EngineKind::LlamaCpp;
    }
    if EngineKind::Candle.is_available() {
        return EngineKind::Candle;
    }
    EngineKind::Mock
}

/// Honest warning when the chosen backend has no engine that can serve it.
pub fn fallback_warning(requested: Backend, resolved: EngineKind) -> Option<String> {
    let has_native = matches!(resolved, EngineKind::LlamaCpp | EngineKind::Candle);
    if requested == Backend::Mock || has_native {
        return None;
    }
    // llama.cpp is the engine that would serve a GPU backend here, and "we did
    // not use it" has two causes worth telling the user apart.
    let reason = EngineKind::LlamaCpp
        .unavailability()
        .unwrap_or("cannot serve requests in this build");
    Some(format!(
        "Nothing in this build runs {} inference: the llama-cpp engine {}, so {} \
         answered instead and every number here is simulated.",
        requested.as_str(),
        reason,
        resolved.as_str()
    ))
}

/// Best backend for a machine: Metal on Apple Silicon, CUDA on NVIDIA, else CPU.
pub fn recommended_backend(hardware: &HardwareReport) -> Backend {
    if hardware.metal_available {
        Backend::Metal
    } else if hardware.nvidia_gpu.is_some() {
        Backend::Cuda
    } else if hardware.vulkan_available {
        Backend::Vulkan
    } else {
        Backend::Cpu
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_resolved_engine_can_always_serve_a_request() {
        for backend in [
            Backend::Cpu,
            Backend::Metal,
            Backend::Cuda,
            Backend::Vulkan,
            Backend::Mock,
        ] {
            let kind = kind_for_backend(backend);
            assert!(
                kind.is_available(),
                "{kind:?} for {backend:?} cannot serve requests in this build"
            );
            assert!(!kind.build().name().is_empty());
        }
        assert_eq!(kind_for_backend(Backend::Mock), EngineKind::Mock);
    }

    #[test]
    fn a_compiled_but_unlinked_adapter_never_displaces_mock() {
        // Building with `--features llama-cpp` compiles the adapter without
        // linking llama.cpp. Resolution must still land on an engine that can
        // answer a load, otherwise the app starts and then fails at once.
        if EngineKind::LlamaCpp.is_linked() {
            return;
        }
        assert_eq!(kind_for_backend(Backend::Metal), EngineKind::Mock);
        assert_eq!(kind_for_backend(Backend::Cuda), EngineKind::Mock);
    }

    #[test]
    fn every_engine_kind_builds() {
        for kind in EngineKind::all() {
            let engine = kind.build();
            assert!(!engine.name().is_empty());
        }
    }

    #[test]
    fn warnings_only_appear_when_the_backend_is_missing() {
        assert!(fallback_warning(Backend::Mock, EngineKind::Mock).is_none());
        assert!(fallback_warning(Backend::Metal, EngineKind::LlamaCpp).is_none());

        let warning = fallback_warning(Backend::Metal, EngineKind::Mock)
            .expect("metal served by mock must warn");
        assert!(warning.contains("mock"), "{warning}");
        assert!(warning.contains("llama-cpp"), "{warning}");
        assert!(warning.contains("simulated"), "{warning}");
        let expected = if EngineKind::LlamaCpp.compiled() {
            "adapter only"
        } else {
            "was not compiled"
        };
        assert!(
            warning.contains(expected),
            "{warning} must name the real cause"
        );
    }

    #[test]
    fn backend_recommendation_follows_the_detected_gpu() {
        let apple = HardwareReport {
            metal_available: true,
            ..Default::default()
        };
        assert_eq!(recommended_backend(&apple), Backend::Metal);

        let nvidia = HardwareReport {
            nvidia_gpu: Some("RTX 4060".to_string()),
            ..Default::default()
        };
        assert_eq!(recommended_backend(&nvidia), Backend::Cuda);

        let plain = HardwareReport::default();
        assert_eq!(recommended_backend(&plain), Backend::Cpu);
    }
}
