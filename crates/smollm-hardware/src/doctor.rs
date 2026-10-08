//! Turns a raw [`HardwareReport`] into a friendly, actionable diagnosis.
//!
//! The doctor deliberately knows nothing about the catalog crate: callers hand
//! it lightweight [`ModelCandidate`] values, so hardware stays testable in
//! isolation and reusable from the CLI.

use serde::{Deserialize, Serialize};
use smollm_core::model::estimate_ram_gb;
use smollm_core::system::{Backend, DoctorReport, HardwareReport, Platform};

/// Fraction of currently free RAM we are willing to hand to one model.
const RAM_HEADROOM: f64 = 0.7;
/// Small models advertise 32K context but rarely run it on a laptop, so RAM
/// sizing assumes a bounded context instead of the advertised ceiling.
const ASSUMED_CONTEXT_TOKENS: u32 = 4096;
/// Parameter bands the app talks about, ascending.
const BANDS_B: [f32; 6] = [0.5, 1.0, 1.7, 2.0, 3.0, 4.0];
/// How many ids land in `recommended_models`.
const RECOMMENDED_LIMIT: usize = 6;

/// A possible recommendation, decoupled from catalog types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelCandidate {
    pub id: String,
    pub parameters_b: f32,
    /// Decimal megabytes, as reported by the model registry.
    pub size_mb: u64,
    pub context_length: u32,
    /// Placeholder catalog entries cannot be downloaded yet.
    pub placeholder: bool,
}

impl Default for ModelCandidate {
    fn default() -> Self {
        Self {
            id: String::new(),
            parameters_b: 0.5,
            size_mb: 400,
            context_length: 4096,
            placeholder: false,
        }
    }
}

impl ModelCandidate {
    /// Estimated resident memory at a conservative context length.
    pub fn ram_needed_gb(&self) -> f64 {
        let context = self.context_length.clamp(512, ASSUMED_CONTEXT_TOKENS);
        estimate_ram_gb(self.size_mb, context)
    }

    pub fn fits(&self, usable_ram_gb: f64) -> bool {
        self.ram_needed_gb() <= usable_ram_gb
    }
}

/// Build the doctor report for a machine and a set of candidate models.
pub fn build_doctor(hardware: &HardwareReport, candidates: &[ModelCandidate]) -> DoctorReport {
    let usable_ram = (hardware.available_ram_gb * RAM_HEADROOM).max(0.0);
    let real: Vec<&ModelCandidate> = candidates.iter().filter(|c| !c.placeholder).collect();

    let max_comfortable = max_comfortable_parameters_b(usable_ram, &real);
    let recommended_models = pick_recommendations(&real, usable_ram, max_comfortable);
    let backend_recommendation = recommend_backend(hardware);
    let warnings = collect_warnings(hardware, candidates, &recommended_models);
    let (headline, detail) = narrative(hardware, band_of(max_comfortable), &recommended_models);

    DoctorReport {
        platform: hardware.platform.id().to_string(),
        arch: hardware.arch.clone(),
        cpu_cores: hardware.logical_cores,
        ram_gb: hardware.total_ram_gb,
        gpu: hardware.gpu_name.clone(),
        backend_recommendation,
        recommended_models,
        warnings,
        headline,
        detail,
        max_comfortable_parameters_b: max_comfortable,
        hardware: hardware.clone(),
    }
}

/// Largest band that fits in RAM, or a RAM-derived fallback when no candidate
/// fits so the UI still has something honest to say.
fn max_comfortable_parameters_b(usable_ram_gb: f64, candidates: &[&ModelCandidate]) -> f32 {
    const MAX_ADVERTISED_PARAMETERS_B: f32 = 4.0;

    let largest = candidates
        .iter()
        .filter(|c| c.fits(usable_ram_gb))
        .map(|c| c.parameters_b)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    match largest {
        Some(params) => band_of(params.min(MAX_ADVERTISED_PARAMETERS_B)),
        None => {
            // Roughly 0.6 GB of weights per billion parameters at Q4_K_M.
            let fallback = (usable_ram_gb * 1.5).clamp(0.0, f64::from(MAX_ADVERTISED_PARAMETERS_B));
            band_of(fallback as f32)
        }
    }
}

/// Snap a parameter count down to one of the supported bands.
fn band_of(parameters_b: f32) -> f32 {
    bands_up_to(parameters_b)
        .last()
        .copied()
        .unwrap_or(BANDS_B[0])
}

fn bands_up_to(parameters_b: f32) -> Vec<f32> {
    BANDS_B
        .iter()
        .copied()
        .filter(|&band| band <= parameters_b + f32::EPSILON)
        .collect()
}

/// "0.5B", "1B", "1.7B", "3B" …
pub fn band_label(parameters_b: f32) -> String {
    if (parameters_b - parameters_b.round()).abs() < 0.05 {
        format!("{}B", parameters_b.round() as i64)
    } else {
        format!("{parameters_b}B")
    }
}

/// The two headline bands for low-end machines, e.g. "0.5B and 1B".
fn band_pair(parameters_b: f32) -> String {
    let allowed = bands_up_to(parameters_b);
    match allowed.len() {
        0 | 1 => band_label(allowed.last().copied().unwrap_or(BANDS_B[0])),
        _ => format!(
            "{} and {}",
            band_label(allowed[allowed.len() - 2]),
            band_label(allowed[allowed.len() - 1])
        ),
    }
}

/// Best-fitting models first, smallest last, so the UI shows quality options
/// before speed demons.
fn pick_recommendations(
    candidates: &[&ModelCandidate],
    usable_ram_gb: f64,
    max_comfortable: f32,
) -> Vec<String> {
    let mut fitting: Vec<&&ModelCandidate> = candidates
        .iter()
        .filter(|c| c.fits(usable_ram_gb) && c.parameters_b <= max_comfortable + f32::EPSILON)
        .collect();
    fitting.sort_by(|a, b| {
        b.parameters_b
            .partial_cmp(&a.parameters_b)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    fitting
        .iter()
        .take(RECOMMENDED_LIMIT)
        .map(|c| c.id.clone())
        .collect()
}

fn recommend_backend(hardware: &HardwareReport) -> Backend {
    match hardware.platform {
        Platform::Macos if hardware.metal_available => Backend::Metal,
        Platform::Windows | Platform::Linux => {
            if hardware.nvidia_gpu.is_some() {
                Backend::Cuda
            } else if hardware.vulkan_available {
                Backend::Vulkan
            } else {
                Backend::Cpu
            }
        }
        _ => Backend::Cpu,
    }
}

fn collect_warnings(
    hardware: &HardwareReport,
    candidates: &[ModelCandidate],
    recommended: &[String],
) -> Vec<String> {
    let mut warnings = Vec::new();

    if hardware.total_ram_gb < 6.0 {
        warnings.push(format!(
            "{:.0} GB of RAM is tight: close other apps and prefer 0.5B models.",
            hardware.total_ram_gb
        ));
    }
    if hardware.available_ram_gb < hardware.total_ram_gb * 0.35 {
        warnings.push(
            "Little memory is currently free. Free up RAM before loading a larger model."
                .to_string(),
        );
    }
    if hardware.model_volume_free_gb < 5.0 {
        warnings.push(format!(
            "Only {:.1} GB free in the model folder. Each small model needs 0.3-2.5 GB.",
            hardware.model_volume_free_gb
        ));
    }
    if hardware.platform == Platform::Macos
        && !hardware.apple_silicon
        && hardware.cpu_brand.to_ascii_lowercase().contains("intel")
    {
        warnings.push(
            "Intel Mac detected: CPU inference is expected to be slow for 3B+ models.".to_string(),
        );
    }
    if hardware.physical_cores > 0 && hardware.physical_cores <= 2 {
        warnings.push(format!(
            "Only {} physical cores detected, so generation will be CPU bound.",
            hardware.physical_cores
        ));
    }
    if recommended.is_empty() {
        if candidates.is_empty() {
            warnings.push("The model catalog is empty, so no recommendation could be made.".into());
        } else if candidates.iter().all(|c| c.placeholder) {
            warnings.push(
                "Every catalog entry is a placeholder, so downloads are not available yet."
                    .to_string(),
            );
        } else {
            warnings.push(
                "No catalog model comfortably fits the free memory right now. Smaller quants such \
                 as Q4_0 or Q3_K_S are your best bet."
                    .to_string(),
            );
        }
    }

    warnings
}

/// Headline + supporting detail, matching the product copy in the spec.
fn narrative(hardware: &HardwareReport, band: f32, recommended: &[String]) -> (String, String) {
    let headline = if band >= 3.0 {
        format!(
            "Your machine looks great for 0.5B–{} models.",
            band_label(band)
        )
    } else if band >= 1.7 {
        format!(
            "Your machine can comfortably run 0.5B–{} models.",
            band_label(band)
        )
    } else {
        format!(
            "{:.0} GB RAM detected. Recommended: {} models with Q4_K_M quantization.",
            hardware.total_ram_gb,
            band_pair(band)
        )
    };

    let backend = recommend_backend(hardware).as_str();
    let mut detail = match hardware.platform {
        Platform::Macos if hardware.apple_silicon => format!(
            "{} with unified memory is a strong fit for small GGUF models; the {backend} backend \
             is recommended.",
            hardware.gpu_name
        ),
        Platform::Macos => format!("Metal is available, so {backend} is the recommended backend."),
        _ if hardware.nvidia_gpu.is_some() => format!(
            "{} detected: {backend} will beat CPU inference on 2B and 3B models.",
            hardware
                .nvidia_gpu
                .clone()
                .unwrap_or_else(|| "An NVIDIA GPU".to_string())
        ),
        _ if hardware.vulkan_available => {
            format!("Vulkan adapter found, so {backend} is worth trying before CPU.")
        }
        _ => format!(
            "No accelerator was detected, so {backend} is the safe choice. Small models stay \
             responsive on CPU."
        ),
    };

    if !recommended.is_empty() {
        detail.push_str(&format!(
            " {} models are ready to download for this machine.",
            recommended.len()
        ));
    }
    if hardware.available_ram_gb < hardware.total_ram_gb * 0.5 {
        detail.push_str(" Free up memory before loading models for the best speed.");
    }

    (headline, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hardware(total: f64, available: f64) -> HardwareReport {
        HardwareReport {
            platform: Platform::Macos,
            arch: "aarch64".to_string(),
            cpu_brand: "Apple M3".to_string(),
            logical_cores: 10,
            physical_cores: 10,
            total_ram_gb: total,
            available_ram_gb: available,
            apple_silicon: true,
            metal_available: true,
            nvidia_gpu: None,
            vulkan_available: false,
            gpu_name: "Apple M3".to_string(),
            disk_free_gb: 200.0,
            model_volume_free_gb: 200.0,
            accelerator: None,
        }
    }

    fn candidate(id: &str, parameters_b: f32, size_mb: u64) -> ModelCandidate {
        ModelCandidate {
            id: id.to_string(),
            parameters_b,
            size_mb,
            context_length: 32768,
            placeholder: false,
        }
    }

    fn sample_candidates() -> Vec<ModelCandidate> {
        vec![
            candidate("tiny", 0.5, 271),
            candidate("qwen-0.5b", 0.5, 491),
            candidate("llama-1b", 1.0, 808),
            candidate("smollm-1.7b", 1.7, 1056),
            candidate("qwen-3b", 3.0, 2105),
            candidate("gemma-4b", 4.0, 2526),
        ]
    }

    #[test]
    fn roomy_machine_gets_an_enthusiastic_headline() {
        let report = build_doctor(&hardware(16.0, 11.0), &sample_candidates());
        assert_eq!(report.max_comfortable_parameters_b, 4.0);
        assert_eq!(
            report.headline,
            "Your machine looks great for 0.5B–4B models."
        );
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.backend_recommendation, Backend::Metal);
        assert_eq!(report.platform, "macos");
        assert_eq!(report.cpu_cores, 10);
        assert_eq!(report.ram_gb, 16.0);
        // Biggest-first ordering puts quality ahead of speed.
        assert_eq!(
            report.recommended_models.first().map(String::as_str),
            Some("gemma-4b")
        );
        assert_eq!(report.recommended_models.len(), 6);
        assert!(report.detail.contains("6 models are ready"));
    }

    #[test]
    fn mid_range_machine_is_praised_less_loudly() {
        let report = build_doctor(&hardware(8.0, 3.5), &sample_candidates());
        assert_eq!(report.max_comfortable_parameters_b, 1.7);
        assert_eq!(
            report.headline,
            "Your machine can comfortably run 0.5B–1.7B models."
        );
    }

    #[test]
    fn tight_ram_recommends_small_models_with_quantization_advice() {
        let report = build_doctor(&hardware(8.0, 2.4), &sample_candidates());
        assert_eq!(report.max_comfortable_parameters_b, 1.0);
        assert_eq!(
            report.headline,
            "8 GB RAM detected. Recommended: 0.5B and 1B models with Q4_K_M quantization."
        );
        assert!(!report.recommended_models.is_empty());
        assert!(report
            .recommended_models
            .iter()
            .all(|id| id == "tiny" || id == "qwen-0.5b" || id == "llama-1b"));
    }

    #[test]
    fn very_low_ram_warns_and_stops_at_05b() {
        let report = build_doctor(&hardware(4.0, 2.0), &sample_candidates());
        assert_eq!(report.max_comfortable_parameters_b, 0.5);
        assert_eq!(
            report.headline,
            "4 GB RAM detected. Recommended: 0.5B models with Q4_K_M quantization."
        );
        assert!(report
            .warnings
            .iter()
            .any(|w| w.contains("GB of RAM is tight")));
    }

    #[test]
    fn empty_catalog_is_reported_as_a_warning() {
        let report = build_doctor(&hardware(16.0, 12.0), &[]);
        assert!(report.recommended_models.is_empty());
        assert!(report
            .warnings
            .iter()
            .any(|w| w.contains("catalog is empty")));
        // Falls back to a RAM-derived sizing so the UI still says something.
        assert!(report.max_comfortable_parameters_b >= 1.7);
    }

    #[test]
    fn placeholders_are_never_recommended() {
        let candidates = vec![
            ModelCandidate {
                placeholder: true,
                ..candidate("ghost", 1.0, 700)
            },
            candidate("real", 0.5, 300),
        ];
        let report = build_doctor(&hardware(16.0, 12.0), &candidates);
        assert_eq!(report.recommended_models, vec!["real".to_string()]);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    }

    #[test]
    fn all_placeholders_produce_a_warning() {
        let candidates = vec![ModelCandidate {
            placeholder: true,
            ..candidate("ghost", 1.0, 700)
        }];
        let report = build_doctor(&hardware(16.0, 12.0), &candidates);
        assert!(
            report.warnings.iter().any(|w| w.contains("placeholder")),
            "{:?}",
            report.warnings
        );
        assert!(report.recommended_models.is_empty());
    }

    #[test]
    fn nothing_fits_when_memory_is_exhausted() {
        let report = build_doctor(&hardware(16.0, 0.4), &sample_candidates());
        assert!(report.recommended_models.is_empty());
        assert!(report
            .warnings
            .iter()
            .any(|w| w.contains("Free up RAM") || w.contains("smaller quants")));
    }

    #[test]
    fn low_disk_space_is_flagged() {
        let mut hw = hardware(16.0, 12.0);
        hw.model_volume_free_gb = 2.0;
        let report = build_doctor(&hw, &sample_candidates());
        assert!(report
            .warnings
            .iter()
            .any(|w| w.contains("free in the model folder")));
    }

    #[test]
    fn backend_recommendation_per_platform() {
        let mut hw = hardware(16.0, 12.0);
        hw.platform = Platform::Windows;
        hw.apple_silicon = false;
        hw.metal_available = false;
        hw.gpu_name = "Unknown GPU (CPU inference)".into();
        assert_eq!(recommend_backend(&hw), Backend::Cpu);

        hw.nvidia_gpu = Some("NVIDIA RTX 3060".into());
        assert_eq!(recommend_backend(&hw), Backend::Cuda);

        hw.nvidia_gpu = None;
        hw.vulkan_available = true;
        assert_eq!(recommend_backend(&hw), Backend::Vulkan);

        hw.platform = Platform::Macos;
        hw.metal_available = true;
        assert_eq!(recommend_backend(&hw), Backend::Metal);
    }

    #[test]
    fn intel_mac_gets_an_intel_warning() {
        let mut hw = hardware(16.0, 12.0);
        hw.apple_silicon = false;
        hw.cpu_brand = "Intel(R) Core(TM) i9-9880H CPU @ 2.30GHz".into();
        hw.gpu_name = "Metal-capable GPU (Intel Mac)".into();
        let report = build_doctor(&hw, &sample_candidates());
        assert!(report
            .warnings
            .iter()
            .any(|w| w.contains("Intel Mac detected")));
    }

    #[test]
    fn bands_snap_downwards_and_labels_read_well() {
        assert_eq!(band_of(3.9), 3.0);
        assert_eq!(band_of(4.0), 4.0);
        assert_eq!(band_of(0.4), 0.5);
        assert_eq!(band_of(2.9), 2.0);
        assert_eq!(band_label(0.5), "0.5B");
        assert_eq!(band_label(1.0), "1B");
        assert_eq!(band_label(1.7), "1.7B");
        assert_eq!(band_pair(1.0), "0.5B and 1B");
        assert_eq!(band_pair(0.5), "0.5B");
        assert_eq!(band_pair(3.0), "2B and 3B");
    }

    #[test]
    fn candidate_ram_estimate_caps_context() {
        let model = candidate("qwen-3b", 3.0, 2105);
        let needed = model.ram_needed_gb();
        assert!(needed > 2.1, "got {needed}");
        assert!(needed < 3.5, "got {needed}");
        // A 32K context would blow the estimate up to ~7 GB, which is not what
        // a laptop user actually runs a 3B model at.
        assert!(
            estimate_ram_gb(2105, 32768) > needed,
            "context cap should lower the estimate"
        );
    }

    #[test]
    fn doctor_is_serialisable_in_camel_case() {
        let report = build_doctor(&hardware(16.0, 12.0), &sample_candidates());
        let json = serde_json::to_value(&report).expect("doctor serialises");
        for key in [
            "backendRecommendation",
            "recommendedModels",
            "maxComfortableParametersB",
            "hardware",
            "headline",
            "warnings",
        ] {
            assert!(json.get(key).is_some(), "missing {key}");
        }
        assert_eq!(json["backendRecommendation"], "metal");
    }
}
