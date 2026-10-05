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
    pub n_tensors: u64,
    pub gguf_version: u32,
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
}
