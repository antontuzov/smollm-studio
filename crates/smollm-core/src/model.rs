//! Catalog and local-model data types.

use serde::{Deserialize, Serialize};

/// Model families we know how to prompt. Keep in sync with `data/catalog.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelFamily {
    Qwen2,
    Llama3,
    #[serde(rename = "smollm2")]
    SmolLm2,
    Phi3,
    Gemma,
    /// IBM Granite 3.x: a Harmony-style template, not ChatML.
    Granite,
    #[default]
    Other,
}

impl ModelFamily {
    pub fn label(self) -> &'static str {
        match self {
            Self::Qwen2 => "Qwen2.5",
            Self::Llama3 => "Llama 3.2",
            Self::SmolLm2 => "SmolLM2",
            Self::Phi3 => "Phi-3",
            Self::Gemma => "Gemma",
            Self::Granite => "Granite",
            Self::Other => "GGUF",
        }
    }

    /// Best-effort family from a GGUF architecture, model id, or filename.
    ///
    /// Used when a caller has no catalog entry; unknown hints fall back to
    /// [`ModelFamily::Other`], whose template is the safest ChatML-lite shape.
    pub fn from_hint(hint: &str) -> Self {
        let lower = hint.to_ascii_lowercase();
        if lower.contains("granite") {
            Self::Granite
        } else if lower.contains("qwen") {
            Self::Qwen2
        } else if lower.contains("smollm") {
            Self::SmolLm2
        } else if lower.contains("llama") {
            Self::Llama3
        } else if lower.contains("phi") {
            Self::Phi3
        } else if lower.contains("gemma") {
            Self::Gemma
        } else {
            Self::Other
        }
    }

    /// Render a chat transcript into a flat prompt for engines without a
    /// tokenizer-aware chat template (mock/simple backends).
    pub fn render_prompt(
        self,
        system: Option<&str>,
        messages: &[crate::chat::ChatMessage],
    ) -> String {
        use crate::chat::Role;
        let mut out = String::new();
        let push = |role_tag: &str, content: &str, out: &mut String| match self {
            Self::Llama3 | Self::SmolLm2 | Self::Qwen2 | Self::Other => {
                out.push_str(&format!("<{role_tag}>\n{content}\n</{role_tag}>\n"));
            }
            Self::Phi3 => out.push_str(&format!("{role_tag}\n{content}\n")),
            Self::Granite => out.push_str(&format!(
                "<|start_of_role|>{role_tag}<|end_of_role|>{content}<|end_of_text|>\n"
            )),
            Self::Gemma => out.push_str(&format!(
                "<start_of_turn>{role_tag}\n{content}<end_of_turn>\n"
            )),
        };

        if let Some(system) = system {
            push("system", system, &mut out);
        }
        for message in messages {
            let tag = match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
            };
            push(tag, &message.content, &mut out);
        }

        match self {
            Self::Phi3 => out.push_str("assistant\n"),
            Self::Gemma => out.push_str("<start_of_turn>model\n"),
            Self::Granite => out.push_str("<|start_of_role|>assistant<|end_of_role|>\n"),
            _ => out.push_str("<assistant>\n"),
        }
        out
    }
}

/// How much we trust a catalog entry's Hugging Face coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CatalogStatus {
    /// Repo + filename confirmed against the Hugging Face API.
    #[default]
    Verified,
    /// Curated entry whose exact GGUF file still needs confirming.
    Placeholder,
}

/// Qualitative 4-level rating used by catalog cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Rating {
    Excellent,
    #[default]
    Good,
    Fair,
    Poor,
}

/// One curated entry of `data/catalog.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelDescriptor {
    pub id: String,
    pub display_name: String,
    pub hf_repo: String,
    pub filename: String,
    /// Hugging Face revision; `main` unless a repo needs a tag.
    pub revision: String,
    pub parameters_b: f32,
    pub quantization: String,
    pub size_mb: u64,
    pub context_length: u32,
    pub vision: bool,
    pub license: String,
    pub family: ModelFamily,
    /// GGUF `general.architecture` (`llama`, `qwen2`, `gemma3`, `phi3`,
    /// `granite`). This is what decides whether an engine can load the file.
    pub architecture: String,
    pub tags: Vec<String>,
    pub recommended_ram_gb: f32,
    pub quality: Rating,
    pub speed: Rating,
    pub status: CatalogStatus,
    pub description: String,
}

impl Default for ModelDescriptor {
    fn default() -> Self {
        Self {
            id: String::new(),
            display_name: String::new(),
            hf_repo: String::new(),
            filename: String::new(),
            revision: "main".to_string(),
            parameters_b: 0.5,
            quantization: "Q4_K_M".to_string(),
            size_mb: 0,
            context_length: 4096,
            vision: false,
            license: "unknown".to_string(),
            family: ModelFamily::default(),
            architecture: String::new(),
            tags: Vec::new(),
            recommended_ram_gb: 4.0,
            quality: Rating::default(),
            speed: Rating::default(),
            status: CatalogStatus::default(),
            description: String::new(),
        }
    }
}

impl ModelDescriptor {
    /// Public Hugging Face resolve URL for the GGUF blob.
    pub fn download_url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.hf_repo, self.revision, self.filename
        )
    }

    /// Hugging Face API endpoint used to verify the file exists.
    pub fn api_url(&self) -> String {
        format!(
            "https://huggingface.co/api/models/{}/resolve/{}?metadata=true",
            self.hf_repo, self.filename
        )
    }

    pub fn hf_page_url(&self) -> String {
        format!("https://huggingface.co/{}", self.hf_repo)
    }

    /// Rough resident-memory footprint: file size plus KV cache headroom.
    /// `size_mb` is decimal MB, as reported by the Hugging Face API.
    pub fn estimated_ram_gb(&self, context_length: u32) -> f64 {
        estimate_ram_gb(self.size_mb, context_length)
    }

    pub fn is_small(&self) -> bool {
        self.parameters_b <= 4.0
    }
}

/// Shared RAM heuristic so the catalog, the hardware doctor and the UI agree.
/// Weights dominate; KV cache scales with context; ~350 MB covers the runtime.
pub fn estimate_ram_gb(size_mb: u64, context_length: u32) -> f64 {
    let weights = size_mb as f64 / 1000.0;
    let kv_cache_gb = f64::from(context_length) * 0.000_12;
    (weights + kv_cache_gb + 0.35).max(weights)
}

/// Bytes in a gibibyte: RAM availability is measured in GiB, so memory fit is
/// too.
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
/// llama.cpp's compute buffers, allocator slack and the app itself, on top of
/// weights plus KV cache.
const RUNTIME_ALLOWANCE_GB: f64 = 0.35;

/// Metadata read from a local GGUF file (or a catalog entry).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelMetadata {
    pub name: Option<String>,
    pub architecture: Option<String>,
    pub quantization: Option<String>,
    pub parameter_count: Option<u64>,
    pub parameters_b: Option<f32>,
    pub context_length: Option<u32>,
    pub train_type: Option<String>,
    pub license: Option<String>,
    pub vocab_size: Option<u64>,
    pub block_count: Option<u32>,
    pub embedding_length: Option<u32>,
    /// Attention heads per layer, from `{arch}.attention.head_count`.
    pub head_count: Option<u32>,
    /// Key/value heads per layer, from `{arch}.attention.head_count_kv`.
    pub head_count_kv: Option<u32>,
    /// Bytes of tensor weights in the file, measured from its own header rather
    /// than estimated from the quantisation name.
    pub weight_bytes: Option<u64>,
    pub n_tensors: u64,
    pub gguf_version: u32,
}

impl ModelMetadata {
    /// Resident memory this model needs at `context_length`, in GiB.
    ///
    /// Built from the file's own numbers: the weight bytes its header reports,
    /// and a KV cache sized by the real attention geometry — K and V, one f16
    /// row per position, per layer and per key/value head. Anything the header
    /// did not say falls back to the coarse [`estimate_ram_gb`] heuristic, so a
    /// catalog entry without a local file still gets a sane figure.
    #[must_use]
    pub fn memory_fit_gb(&self, context_length: u32, size_mb: u64) -> f64 {
        match self.measured_fit_gb(context_length) {
            Some(measured) => measured,
            None => estimate_ram_gb(size_mb, context_length),
        }
    }

    fn measured_fit_gb(&self, context_length: u32) -> Option<f64> {
        let weights = self.weight_bytes? as f64 / GIB;
        let kv_cache = self.kv_cache_gb(context_length)?;
        Some(weights + kv_cache + RUNTIME_ALLOWANCE_GB)
    }

    /// GiB the KV cache occupies at this context length, or `None` when the
    /// header does not describe the attention layers well enough to size it.
    #[must_use]
    pub fn kv_cache_gb(&self, context_length: u32) -> Option<f64> {
        let layers = u64::from(self.block_count?);
        let heads = u64::from(self.head_count?);
        // Models without grouped-query attention repeat the query head count.
        let kv_heads = u64::from(self.head_count_kv.unwrap_or(self.head_count?));
        let embedding = u64::from(self.embedding_length?);
        // GGUF stores no head_dim; llama.cpp derives it from the query heads.
        let head_dim = embedding.checked_div(heads)?;
        if layers == 0 || kv_heads == 0 || head_dim == 0 {
            return None;
        }
        // Two caches (K and V) of f16 elements, one row per context position.
        let bytes = 2u64
            .saturating_mul(u64::from(context_length))
            .saturating_mul(layers)
            .saturating_mul(kv_heads)
            .saturating_mul(head_dim)
            .saturating_mul(2);
        Some(bytes as f64 / GIB)
    }
}

/// A `.gguf` file found in the local model directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModel {
    /// Catalog id when the filename matches a curated entry.
    pub catalog_id: Option<String>,
    pub file_name: String,
    pub path: String,
    pub size_bytes: u64,
    pub modified_ms: u64,
    pub metadata: ModelMetadata,
    /// Set when the file could not be parsed as GGUF.
    pub parse_error: Option<String>,
}

impl LocalModel {
    pub fn display_name(&self) -> String {
        self.metadata
            .name
            .clone()
            .unwrap_or_else(|| self.file_name.clone())
    }

    pub fn model_id(&self) -> String {
        self.catalog_id
            .clone()
            .unwrap_or_else(|| self.file_name.trim_end_matches(".gguf").to_string())
    }
}

/// Outcome of one integrity question asked of a local file.
///
/// `skipped` is a state of its own because GGUF carries no per-file checksum and
/// a header does not always declare its tensor list. Marking something we could
/// not measure as passed would tell the user their model is fine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
    Skipped,
}

/// A single check, named the way it reads in the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationCheck {
    pub label: String,
    pub status: CheckStatus,
    /// What was measured, in units the user can act on.
    pub detail: String,
}

impl VerificationCheck {
    pub fn passed(label: &str, detail: impl Into<String>) -> Self {
        Self::with(label, CheckStatus::Passed, detail)
    }

    pub fn failed(label: &str, detail: impl Into<String>) -> Self {
        Self::with(label, CheckStatus::Failed, detail)
    }

    pub fn skipped(label: &str, detail: impl Into<String>) -> Self {
        Self::with(label, CheckStatus::Skipped, detail)
    }

    fn with(label: &str, status: CheckStatus, detail: impl Into<String>) -> Self {
        Self {
            label: label.to_string(),
            status,
            detail: detail.into(),
        }
    }

    pub fn is_failed(&self) -> bool {
        self.status == CheckStatus::Failed
    }
}

/// The integrity report for one file in the model directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelVerification {
    pub file_name: String,
    pub path: String,
    /// No check failed. Skipped checks do not make a model broken.
    pub ok: bool,
    pub checks: Vec<VerificationCheck>,
}

impl ModelVerification {
    pub fn new(file_name: String, path: String, checks: Vec<VerificationCheck>) -> Self {
        let ok = !checks.iter().any(|check| check.is_failed());
        Self {
            file_name,
            path,
            ok,
            checks,
        }
    }

    /// Checks that came back broken, which is what the UI counts.
    pub fn failures(&self) -> usize {
        self.checks.iter().filter(|check| check.is_failed()).count()
    }
}

/// What pointing the app at a new model folder did to the files in the old one.
///
/// A move never overwrites a name the new folder already holds, and never
/// deletes the old folder, so every file has to be accounted for here: relocated,
/// left where it was, or refused with a reason. Anything in `conflicts`,
/// `duplicates` or `failures` is a file the user still has to decide about.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Relocation {
    pub from: String,
    pub to: String,
    /// Files that moved by rename, which costs no time or space on one volume.
    pub moved: usize,
    /// Files copied byte for byte, then removed from the source, because a
    /// rename cannot cross a volume.
    pub copied: usize,
    /// Bytes that ended up in the new folder.
    pub bytes: u64,
    /// Names the new folder already held at the same size, so the old copy stayed
    /// where it was. Equal length is not proof of equal bytes, which is why these
    /// are listed rather than quietly discarded.
    pub duplicates: Vec<String>,
    /// Names the new folder held at a *different* size. Both files are kept and
    /// the old one is reported, because guessing which is right is not this
    /// code's call.
    pub conflicts: Vec<String>,
    /// Files that could not be relocated, each with the reason.
    pub failures: Vec<String>,
}

impl Relocation {
    /// Files that ended up in the new folder.
    pub fn relocated(&self) -> usize {
        self.moved + self.copied
    }

    /// True when nothing was left behind, so the old folder can be ignored.
    pub fn is_complete(&self) -> bool {
        self.duplicates.is_empty() && self.conflicts.is_empty() && self.failures.is_empty()
    }

    /// Names of the files still sitting in the old folder, which is what the UI
    /// lists so nothing is silently left behind.
    pub fn left_behind(&self) -> Vec<&str> {
        self.duplicates
            .iter()
            .chain(self.conflicts.iter())
            .chain(self.failures.iter())
            .map(String::as_str)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{chat_message, Role};

    #[test]
    fn urls_are_well_formed() {
        let model = ModelDescriptor {
            id: "qwen2.5-0.5b-instruct-gguf".into(),
            hf_repo: "Qwen/Qwen2.5-0.5B-Instruct-GGUF".into(),
            filename: "qwen2.5-0.5b-instruct-q4_k_m.gguf".into(),
            ..Default::default()
        };
        assert_eq!(
            model.download_url(),
            "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/main/qwen2.5-0.5b-instruct-q4_k_m.gguf"
        );
        assert!(model.is_small());
    }

    const MIB: u64 = 1024 * 1024;

    /// Qwen2.5-0.5B: 24 layers, 14 query heads, 2 kv heads, 896 wide.
    fn qwen2_metadata() -> ModelMetadata {
        ModelMetadata {
            block_count: Some(24),
            embedding_length: Some(896),
            head_count: Some(14),
            head_count_kv: Some(2),
            weight_bytes: Some(350 * MIB),
            ..ModelMetadata::default()
        }
    }

    #[test]
    fn kv_cache_is_sized_by_the_real_attention_geometry() {
        let metadata = qwen2_metadata();
        // 2 caches x 4096 positions x 24 layers x 2 kv heads x 64 head_dim x 2 B
        // = 48 MiB, which is what llama.cpp reserves for this model at 4K and
        // what the flat per-token heuristic misses.
        let expected = 48.0 * MIB as f64 / (1024.0 * MIB as f64);
        assert!((metadata.kv_cache_gb(4096).expect("sized") - expected).abs() < 1e-9);
        assert!(
            metadata.kv_cache_gb(8192).unwrap() > metadata.kv_cache_gb(4096).unwrap(),
            "more context means a bigger cache"
        );
    }

    #[test]
    fn a_model_without_grouped_query_attention_sizes_every_head() {
        let mut metadata = qwen2_metadata();
        metadata.head_count_kv = None;
        let shared = metadata.kv_cache_gb(1024).expect("sized");
        metadata.head_count_kv = Some(14);
        assert_eq!(
            Some(shared),
            Some(metadata.kv_cache_gb(1024).expect("sized")),
            "no kv head count must not shrink the cache"
        );
    }

    #[test]
    fn memory_fit_prefers_measured_weights_and_falls_back_to_the_heuristic() {
        let metadata = qwen2_metadata();
        let fit = metadata.memory_fit_gb(4096, 400);
        let weights = 350.0 * MIB as f64 / (1024.0 * MIB as f64);
        let kv_cache = metadata.kv_cache_gb(4096).expect("sized");
        assert!(
            (fit - (weights + kv_cache + 0.35)).abs() < 1e-9,
            "measured weights plus cache plus a runtime allowance"
        );

        // No header: the catalog heuristic is what the app has to offer.
        let mut blind = qwen2_metadata();
        blind.weight_bytes = None;
        assert_eq!(
            blind.memory_fit_gb(4096, 400),
            estimate_ram_gb(400, 4096),
            "an unmeasurable file must still be sized"
        );

        // Geometry missing: same fallback, not a confident wrong number.
        let mut no_heads = qwen2_metadata();
        no_heads.head_count = None;
        assert_eq!(
            no_heads.memory_fit_gb(4096, 400),
            estimate_ram_gb(400, 4096)
        );
    }

    #[test]
    fn prompt_rendering_per_family() {
        let messages = vec![chat_message(Role::User, "hi")];
        let llama = ModelFamily::Llama3.render_prompt(Some("be brief"), &messages);
        assert!(llama.contains("<system>\nbe brief\n</system>"));
        assert!(llama.ends_with("<assistant>\n"));

        let gemma = ModelFamily::Gemma.render_prompt(None, &messages);
        assert!(gemma.contains("<start_of_turn>user\nhi<end_of_turn>"));
        assert!(gemma.ends_with("<start_of_turn>model\n"));

        // Markers copied from the chat_template embedded in Granite's own GGUF.
        let granite = ModelFamily::Granite.render_prompt(None, &messages);
        assert!(granite.contains("<|start_of_role|>user<|end_of_role|>hi<|end_of_text|>"));
        assert!(granite.ends_with("<|start_of_role|>assistant<|end_of_role|>\n"));
        assert_eq!(ModelFamily::from_hint("granite"), ModelFamily::Granite);
    }

    #[test]
    fn ram_estimate_grows_with_context() {
        let model = ModelDescriptor {
            size_mb: 400,
            ..Default::default()
        };
        assert!(model.estimated_ram_gb(32768) > model.estimated_ram_gb(2048));
    }

    #[test]
    fn a_relocation_only_counts_as_clean_with_nothing_left_behind() {
        let mut report = Relocation {
            from: "/old".into(),
            to: "/new".into(),
            moved: 2,
            copied: 1,
            bytes: 300,
            ..Relocation::default()
        };
        assert_eq!(report.relocated(), 3);
        assert!(report.is_complete());

        // A file the new folder already held is a decision the user still has.
        report.conflicts.push("Model-Q4_K_M.gguf".into());
        assert!(!report.is_complete());
        assert_eq!(report.left_behind(), vec!["Model-Q4_K_M.gguf"]);
    }
}
