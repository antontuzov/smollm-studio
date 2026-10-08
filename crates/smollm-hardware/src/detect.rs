//! Best-effort platform probing built on `sysinfo` plus a few shell queries.
//!
//! Detection never fails: anything we cannot measure becomes a conservative
//! default, and the doctor report carries a warning instead of an error.

use std::path::Path;

use crate::gpu;
use smollm_core::paths::AppPaths;
use smollm_core::system::{HardwareReport, Platform};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, System};

/// Bytes per GiB, used for every RAM/disk figure in this crate.
const GIB: f64 = 1_073_741_824.0;

/// Detect against the platform default model directory.
pub fn detect() -> HardwareReport {
    detect_in(&AppPaths::default())
}

/// Detect with an explicit model directory (used by tests and portable installs).
pub fn detect_in(paths: &AppPaths) -> HardwareReport {
    let mut system = System::new();
    system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
    system.refresh_cpu_list(CpuRefreshKind::nothing().with_frequency());

    let total_ram_bytes = system.total_memory();
    let available_ram_bytes = system.available_memory().min(total_ram_bytes);
    let logical_cores = system.cpus().len().max(
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
    );
    let physical_cores = system.physical_core_count().unwrap_or(0);
    let cpu_brand = cpu_brand(&system);
    let arch = std::env::consts::ARCH.to_string();
    let platform = Platform::current();

    let apple_silicon = platform == Platform::Macos && is_apple_silicon(&cpu_brand, &arch);
    // Metal is present on every macOS version SmolLLM Studio targets, including
    // Intel Macs with supported GPUs; llama.cpp builds for both.
    let metal_available = platform == Platform::Macos;
    let nvidia_gpu = gpu::detect_nvidia(&system);
    let vulkan_available = gpu::detect_vulkan(&system);
    let gpu_name = gpu::describe_gpu(
        platform,
        &cpu_brand,
        apple_silicon,
        nvidia_gpu.as_deref(),
        vulkan_available,
    );

    let disk_free_gb = free_space_gb(Path::new("/"));
    let model_volume_free_gb = free_space_gb(&paths.models_dir)
        .or(disk_free_gb)
        .unwrap_or(0.0);

    HardwareReport {
        platform,
        arch,
        cpu_brand: if cpu_brand.is_empty() {
            "Unknown CPU".to_string()
        } else {
            cpu_brand
        },
        logical_cores,
        physical_cores,
        total_ram_gb: bytes_to_gb(total_ram_bytes),
        available_ram_gb: bytes_to_gb(available_ram_bytes),
        apple_silicon,
        metal_available,
        nvidia_gpu,
        vulkan_available,
        gpu_name,
        disk_free_gb: disk_free_gb.unwrap_or(model_volume_free_gb),
        model_volume_free_gb,
        // Only an engine that links a GPU runtime can name its device budget,
        // and this crate deliberately does not; the app fills this in.
        accelerator: None,
    }
}

/// CPU brand string, preferring `sysinfo` and falling back to environment hints.
fn cpu_brand(system: &System) -> String {
    if let Some(brand) = system
        .cpus()
        .first()
        .map(|cpu| cpu.brand().trim().to_string())
    {
        if !brand.is_empty() {
            return collapse_spaces(&brand);
        }
    }
    for key in ["PROCESSOR_IDENTIFIER", "CPU_BRAND"] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                return collapse_spaces(value.trim());
            }
        }
    }
    String::new()
}

/// Free space in GB for a path, `None` when the OS cannot tell us.
pub fn free_space_gb(path: &Path) -> Option<f64> {
    let mut cursor = Some(path.to_path_buf());
    while let Some(candidate) = cursor {
        if candidate.exists() {
            if let Ok(space) = fs2::available_space(&candidate) {
                return Some(space as f64 / GIB);
            }
        }
        cursor = candidate.parent().map(std::path::PathBuf::from);
    }
    None
}

pub fn bytes_to_gb(bytes: u64) -> f64 {
    round1(bytes as f64 / GIB)
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// `Apple M3` on arm64 means Apple Silicon; `Intel …` on x86_64 does not.
pub fn is_apple_silicon(brand: &str, arch: &str) -> bool {
    let brand = brand.to_ascii_lowercase();
    arch == "aarch64"
        && (brand.is_empty() || brand.contains("apple m") || brand.contains("apple silicon"))
}

fn collapse_spaces(value: impl AsRef<str>) -> String {
    value
        .as_ref()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Current process resident memory in MB, when the OS tells us.
pub fn current_memory_mb() -> Option<f64> {
    let pid = sysinfo::Pid::from(std::process::id() as usize);
    let mut system = System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), false);
    system
        .process(pid)
        // `Process::memory` is bytes in sysinfo 0.33.
        .map(|process| process.memory() as f64 / (1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_returns_sane_numbers() {
        let report = detect_in(&AppPaths::default());
        assert_eq!(report.platform, Platform::current());
        assert!(!report.arch.is_empty());
        assert!(report.logical_cores >= 1);
        assert!(report.total_ram_gb > 0.0, "ram should be detected");
        assert!(report.available_ram_gb <= report.total_ram_gb);
        assert!(report.disk_free_gb > 0.0);
        assert!(report.model_volume_free_gb > 0.0);
        assert!(!report.gpu_name.is_empty());
    }

    #[test]
    fn byte_conversion_rounds_to_one_decimal() {
        assert_eq!(bytes_to_gb(16 * 1_073_741_824), 16.0);
        assert_eq!(bytes_to_gb(1_073_741_824 / 2), 0.5);
        assert_eq!(bytes_to_gb(0), 0.0);
        assert_eq!(bytes_to_gb(34_359_738_368), 32.0);
    }

    #[test]
    fn apple_silicon_detection() {
        assert!(is_apple_silicon("Apple M3 Pro", "aarch64"));
        assert!(is_apple_silicon("apple m1", "aarch64"));
        assert!(!is_apple_silicon("Intel(R) Core(TM) i9-9880H", "x86_64"));
        assert!(!is_apple_silicon("Apple M2", "x86_64"));
        // Unknown brand on arm64 macOS is still treated as Apple Silicon.
        assert!(is_apple_silicon("", "aarch64"));
    }

    #[test]
    fn whitespace_in_brand_strings_is_collapsed() {
        assert_eq!(collapse_spaces("  Intel   Core   i9 "), "Intel Core i9");
    }

    #[test]
    fn free_space_falls_back_to_ancestors() {
        let missing = std::env::temp_dir().join("smollm-definitely-not-here");
        assert!(free_space_gb(&missing).is_some());
    }

    #[test]
    fn current_process_memory_is_plausible() {
        if let Some(mb) = current_memory_mb() {
            assert!(mb > 0.0, "measured {mb} MB");
            assert!(mb < 65_536.0);
        }
    }
}
