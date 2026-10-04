//! Hardware detection: what this machine can run, and how confidently.

pub mod detect;
pub mod doctor;
pub mod gpu;

pub use detect::{detect, detect_in};
pub use doctor::{build_doctor, ModelCandidate};

use smollm_core::system::{DoctorReport, HardwareReport};

/// Convenience used by the CLI and the desktop app: detect and summarise.
pub fn doctor(candidates: &[ModelCandidate]) -> DoctorReport {
    build_doctor(&detect(), candidates)
}

/// Detect, summarise, and reuse an already-collected hardware report.
pub fn doctor_with(hardware: &HardwareReport, candidates: &[ModelCandidate]) -> DoctorReport {
    build_doctor(hardware, candidates)
}
