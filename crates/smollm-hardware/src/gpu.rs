//! GPU capability probing.
//!
//! Every probe is optional and must degrade to `None`/`false`: a missing
//! `nvidia-smi` is normal, not an error.

use std::process::Command;

use smollm_core::system::Platform;

/// Run a helper command quietly; `None` on any failure.
fn run(program: &str, args: &[&str]) -> Option<String> {
    let mut command = Command::new(program);
    command.args(args);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW keeps a console from flashing behind the app window.
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// NVIDIA discrete GPU name, when `nvidia-smi` is on PATH.
pub fn detect_nvidia(_system: &sysinfo::System) -> Option<String> {
    if cfg!(target_os = "macos") {
        // No NVIDIA GPUs ship in Macs sold this century.
        return None;
    }
    run("nvidia-smi", &["--query-gpu=name", "--format=csv,noheader"])
        .and_then(|output| parse_nvidia_smi(&output))
}

/// Vulkan adapters are what llama.cpp's Vulkan backend would use.
pub fn detect_vulkan(_system: &sysinfo::System) -> bool {
    if cfg!(target_os = "macos") {
        // Only via MoltenVK, which the app cannot assume is installed.
        return false;
    }
    run("vulkaninfo", &["--summary"]).is_some() || run("vulkaninfo-1-0", &["--summary"]).is_some()
}

/// One human-readable GPU label for the Home page.
pub fn describe_gpu(
    platform: Platform,
    cpu_brand: &str,
    apple_silicon: bool,
    nvidia: Option<&str>,
    vulkan: bool,
) -> String {
    if apple_silicon {
        return apple_gpu_name(cpu_brand).unwrap_or_else(|| "Apple Silicon GPU".to_string());
    }
    if let Some(nvidia) = nvidia {
        return nvidia.to_string();
    }
    match platform {
        Platform::Macos => "Metal-capable GPU (Intel Mac)".to_string(),
        Platform::Windows if vulkan => "Vulkan-capable GPU".to_string(),
        Platform::Windows => "Unknown GPU (CPU inference)".to_string(),
        Platform::Linux if vulkan => "Vulkan-capable GPU".to_string(),
        Platform::Linux => "Unknown GPU (CPU inference)".to_string(),
        Platform::Other => "Unknown GPU".to_string(),
    }
}

/// `Apple M3 Pro` stays as-is; `Apple M3 Max 16-core GPU` is trimmed to the SoC name.
pub fn apple_gpu_name(cpu_brand: &str) -> Option<String> {
    let trimmed = cpu_brand.trim();
    if trimmed.is_empty() || !trimmed.to_ascii_lowercase().starts_with("apple") {
        return None;
    }
    let kept: Vec<&str> = trimmed
        .split_whitespace()
        .filter(|word| {
            !matches!(
                word.to_ascii_lowercase().as_str(),
                "gpu" | "core" | "16-core" | "32-core" | "24-core"
            )
        })
        .take(3)
        .collect();
    if kept.is_empty() {
        return None;
    }
    Some(
        kept.iter()
            .map(|word| capitalise(word))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Normalise one label: `m3` becomes `M3`, `pro` becomes `Pro`.
fn capitalise(word: &str) -> String {
    let lower = word.to_ascii_lowercase();
    if let Some(digits) = lower.strip_prefix('m') {
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            return format!("M{digits}");
        }
    }
    let mut chars = lower.chars();
    let first = chars
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    format!("{first}{}", chars.as_str())
}

/// First GPU name in `nvidia-smi --format=csv,noheader` output.
pub fn parse_nvidia_smi(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nvidia_smi_output() {
        assert_eq!(
            parse_nvidia_smi("NVIDIA GeForce RTX 4060 Laptop GPU\n"),
            Some("NVIDIA GeForce RTX 4060 Laptop GPU".to_string())
        );
        assert_eq!(
            parse_nvidia_smi("\n  NVIDIA RTX A2000 12GB\nNVIDIA RTX A2000\n"),
            Some("NVIDIA RTX A2000 12GB".to_string())
        );
        assert_eq!(parse_nvidia_smi("   \n\n"), None);
        assert_eq!(parse_nvidia_smi(""), None);
    }

    #[test]
    fn apple_gpu_names_are_normalised() {
        assert_eq!(
            apple_gpu_name("Apple M3 Pro").as_deref(),
            Some("Apple M3 Pro")
        );
        assert_eq!(
            apple_gpu_name("Apple M1 Max 32-core GPU").as_deref(),
            Some("Apple M1 Max")
        );
        assert_eq!(apple_gpu_name("Intel(R) Core(TM) i9").as_deref(), None);
        assert_eq!(apple_gpu_name("").as_deref(), None);
    }

    #[test]
    fn gpu_description_covers_every_platform_branch() {
        assert_eq!(
            describe_gpu(Platform::Macos, "Apple M2", true, None, false),
            "Apple M2"
        );
        assert_eq!(
            describe_gpu(
                Platform::Windows,
                "Intel i7",
                false,
                Some("NVIDIA RTX 3060"),
                false
            ),
            "NVIDIA RTX 3060"
        );
        assert_eq!(
            describe_gpu(Platform::Windows, "Intel i7", false, None, true),
            "Vulkan-capable GPU"
        );
        assert_eq!(
            describe_gpu(Platform::Windows, "Intel i7", false, None, false),
            "Unknown GPU (CPU inference)"
        );
        assert_eq!(
            describe_gpu(Platform::Linux, "Ryzen", false, None, true),
            "Vulkan-capable GPU"
        );
        assert_eq!(
            describe_gpu(Platform::Macos, "Intel Core i9", false, None, false),
            "Metal-capable GPU (Intel Mac)"
        );
        assert_eq!(
            describe_gpu(Platform::Other, "", false, None, false),
            "Unknown GPU"
        );
    }

    #[test]
    fn probing_helper_binaries_does_not_panic_when_absent() {
        // Absent tooling must yield None/false rather than panicking.
        assert!(run("definitely-not-a-real-binary-xyz", &["--help"]).is_none());
        assert!(parse_nvidia_smi("kept for coverage").is_some());
    }
}
