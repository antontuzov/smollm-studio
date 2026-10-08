//! GGUF metadata backend.
//!
//! Reads real GGUF headers so the Library, Models and Chat pages can show true
//! architecture, quantisation and context information without any inference
//! engine being compiled in. Generation is honestly refused.

use smollm_core::chat::{
    EngineMetrics, GenerationRequest, LoadModelOptions, LoadModelRequest, ModelHandle,
};
use smollm_core::gguf::GgufHeader;
use smollm_core::{AppError, AppResult};

use crate::{Engine, TokenStream};

/// Metadata-only engine: it inspects files, it does not run them.
#[derive(Debug, Default)]
pub struct GgufMetadataEngine {
    loaded: Option<ModelHandle>,
    options: LoadModelOptions,
    read_bytes: u64,
}

impl GgufMetadataEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes of header metadata read during the last load, for the Library UI.
    pub fn metadata_bytes_read(&self) -> u64 {
        self.read_bytes
    }
}

impl Engine for GgufMetadataEngine {
    fn name(&self) -> &str {
        "gguf-metadata"
    }

    fn supports_gguf(&self) -> bool {
        true
    }

    fn is_simulated(&self) -> bool {
        // Nothing is inferred, so any answer from this engine would be fake.
        true
    }

    fn load_model(&mut self, request: LoadModelRequest) -> AppResult<ModelHandle> {
        if request.path.as_os_str().is_empty() {
            return Err(AppError::ModelNotDownloaded(request.model_id));
        }
        if !request.path.exists() {
            return Err(AppError::ModelNotDownloaded(format!(
                "{} ({})",
                request.model_id,
                request.path.display()
            )));
        }

        let header = GgufHeader::read(&request.path)?;
        self.read_bytes = request
            .path
            .metadata()
            .map(|meta| meta.len().min(1_048_576))
            .unwrap_or(0);

        let mut metadata = header.summary();
        if let Some(catalog) = request.metadata {
            merge_metadata(&mut metadata, &catalog);
        }

        let context_length = pick_context(metadata.context_length, &request.options);
        let handle = ModelHandle {
            id: uuid::Uuid::new_v4().simple().to_string(),
            model_id: request.model_id.clone(),
            display_name: if request.display_name.is_empty() {
                metadata
                    .name
                    .clone()
                    .unwrap_or_else(|| request.model_id.clone())
            } else {
                request.display_name
            },
            path: request.path.display().to_string(),
            engine: self.name().to_string(),
            context_length,
            metadata,
        };

        self.options = request.options;
        self.loaded = Some(handle.clone());
        Ok(handle)
    }

    fn unload_model(&mut self, handle: ModelHandle) -> AppResult<()> {
        if self
            .loaded
            .as_ref()
            .is_some_and(|loaded| loaded.id == handle.id)
        {
            self.loaded = None;
        }
        Ok(())
    }

    fn generate(&mut self, _request: GenerationRequest) -> AppResult<TokenStream> {
        Err(AppError::NotImplemented(
            "the GGUF metadata backend only reads files; it has no runtime to generate with",
        ))
    }

    fn tokenize(&self, _text: &str) -> AppResult<Vec<u32>> {
        Err(AppError::NotImplemented(
            "tokenisation requires a real engine; the metadata backend has no vocabulary",
        ))
    }

    fn metrics(&self) -> EngineMetrics {
        EngineMetrics {
            engine: self.name().to_string(),
            simulated: true,
            model_id: self.loaded.as_ref().map(|handle| handle.model_id.clone()),
            requests: 0,
            tokens_generated: 0,
            total_generation_ms: 0,
            tokens_per_second: 0.0,
            loaded: self.loaded.is_some(),
            backend: self.options.backend,
        }
    }
}

/// The model's own context ceiling wins unless the user asked for something
/// smaller, so the UI never promises more than the file can do.
pub(crate) fn pick_context(from_file: Option<u32>, options: &LoadModelOptions) -> u32 {
    let requested = options.context_length.max(1);
    match from_file {
        Some(max) if max > 0 => requested.min(max),
        _ => requested,
    }
}

/// Fill gaps in the GGUF-derived metadata with catalog values.
pub(crate) fn merge_metadata(
    target: &mut smollm_core::model::ModelMetadata,
    fallback: &smollm_core::model::ModelMetadata,
) {
    let smollm_core::model::ModelMetadata {
        name,
        architecture,
        quantization,
        parameter_count,
        parameters_b,
        context_length,
        train_type,
        license,
        vocab_size,
        block_count,
        embedding_length,
        head_count,
        head_count_kv,
        weight_bytes,
        n_tensors,
        gguf_version,
    } = target;

    if name.is_none() {
        *name = fallback.name.clone();
    }
    if architecture.is_none() {
        *architecture = fallback.architecture.clone();
    }
    if quantization.is_none() {
        *quantization = fallback.quantization.clone();
    }
    if parameter_count.is_none() {
        *parameter_count = fallback.parameter_count;
    }
    if parameters_b.is_none() {
        *parameters_b = fallback.parameters_b;
    }
    if context_length.is_none() {
        *context_length = fallback.context_length;
    }
    if train_type.is_none() {
        *train_type = fallback.train_type.clone();
    }
    if license.is_none() {
        *license = fallback.license.clone();
    }
    if vocab_size.is_none() {
        *vocab_size = fallback.vocab_size;
    }
    if block_count.is_none() {
        *block_count = fallback.block_count;
    }
    if embedding_length.is_none() {
        *embedding_length = fallback.embedding_length;
    }
    if head_count.is_none() {
        *head_count = fallback.head_count;
    }
    if head_count_kv.is_none() {
        *head_count_kv = fallback.head_count_kv;
    }
    // A catalog entry cannot know what the tensor section of a local file
    // weighs, so a missing measurement stays missing rather than guessed.
    let _ = (weight_bytes, n_tensors, gguf_version);
}

#[cfg(test)]
mod tests {
    use super::*;
    use smollm_core::system::Backend;
    use std::io::Write;

    /// Minimal but valid GGUF v3 header with one string KV pair.
    fn gguf_bytes(architecture: Option<&str>, context_length: Option<u32>) -> Vec<u8> {
        fn push_string(out: &mut Vec<u8>, value: &str) {
            out.extend_from_slice(&(value.len() as u64).to_le_bytes());
            out.extend_from_slice(value.as_bytes());
        }
        fn push_kv_string(out: &mut Vec<u8>, key: &str, value: &str) {
            push_string(out, key);
            out.extend_from_slice(&8u32.to_le_bytes()); // STRING
            push_string(out, value);
        }
        fn push_kv_u32(out: &mut Vec<u8>, key: &str, value: u32) {
            push_string(out, key);
            out.extend_from_slice(&4u32.to_le_bytes()); // UINT32
            out.extend_from_slice(&value.to_le_bytes());
        }

        let mut pairs: Vec<u8> = Vec::new();
        let mut n_kv = 0u64;
        if let Some(architecture) = architecture {
            push_kv_string(&mut pairs, "general.architecture", architecture);
            n_kv += 1;
        }
        if let Some(context_length) = context_length {
            // Real GGUF files key context length by architecture, so the
            // fixture has to match its own `general.architecture` value.
            let prefix = architecture.unwrap_or("llama");
            push_kv_u32(
                &mut pairs,
                &format!("{prefix}.context_length"),
                context_length,
            );
            n_kv += 1;
        }
        push_kv_string(&mut pairs, "general.name", "Fixture Model");
        n_kv += 1;

        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"GGUF");
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes()); // n_tensors
        out.extend_from_slice(&n_kv.to_le_bytes());
        out.extend_from_slice(&pairs);
        out
    }

    fn write_fixture(dir: &tempfile::TempDir, bytes: Vec<u8>) -> std::path::PathBuf {
        let path = dir.path().join("fixture.gguf");
        let mut file = std::fs::File::create(&path).expect("create fixture");
        file.write_all(&bytes).expect("write fixture");
        path
    }

    #[test]
    fn reads_real_metadata_from_a_gguf_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_fixture(&dir, gguf_bytes(Some("qwen2"), Some(32768)));

        let mut engine = GgufMetadataEngine::new();
        let handle = engine
            .load_model(LoadModelRequest {
                model_id: "fixture".into(),
                display_name: String::new(),
                path,
                options: LoadModelOptions {
                    context_length: 4096,
                    gpu_layers: -1,
                    backend: Backend::Metal,
                    threads: None,
                },
                metadata: None,
            })
            .expect("loads");

        assert_eq!(handle.engine, "gguf-metadata");
        assert_eq!(handle.model_id, "fixture");
        assert_eq!(handle.display_name, "Fixture Model");
        assert_eq!(handle.context_length, 4096);
        assert_eq!(handle.metadata.architecture.as_deref(), Some("qwen2"));
        assert_eq!(handle.metadata.context_length, Some(32768));
        assert!(engine.metrics().loaded);
        assert_eq!(engine.metrics().backend, Backend::Metal);
    }

    #[test]
    fn context_is_clamped_to_what_the_file_supports() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_fixture(&dir, gguf_bytes(Some("llama"), Some(2048)));

        let mut engine = GgufMetadataEngine::new();
        let handle = engine
            .load_model(LoadModelRequest {
                model_id: "small-context".into(),
                options: LoadModelOptions {
                    context_length: 8192,
                    ..LoadModelOptions::default()
                },
                path,
                ..LoadModelRequest::default()
            })
            .expect("loads");
        assert_eq!(handle.context_length, 2048);
    }

    #[test]
    fn refuses_to_generate_because_it_cannot_infer() {
        let mut engine = GgufMetadataEngine::new();
        let error = engine
            .generate(GenerationRequest {
                request_id: "r".into(),
                prompt: "hi".into(),
                params: smollm_core::chat::SamplingParams::default(),
                stop: Vec::new(),
                cancel: smollm_core::chat::CancelToken::new(),
            })
            .err()
            .expect("metadata engine cannot generate");
        assert!(matches!(error, AppError::NotImplemented(_)));
        assert!(engine.tokenize("hello").is_err());
        assert_eq!(error.code(), "not_implemented");
    }

    #[test]
    fn missing_files_are_reported_as_not_downloaded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = GgufMetadataEngine::new();

        let error = engine
            .load_model(LoadModelRequest {
                model_id: "ghost".into(),
                path: dir.path().join("nope.gguf"),
                ..LoadModelRequest::default()
            })
            .expect_err("file missing");
        assert!(matches!(error, AppError::ModelNotDownloaded(_)));

        let error = engine
            .load_model(LoadModelRequest {
                model_id: "ghost".into(),
                ..LoadModelRequest::default()
            })
            .expect_err("no path given");
        assert!(matches!(error, AppError::ModelNotDownloaded(_)));
    }

    #[test]
    fn corrupt_files_surface_a_parse_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_fixture(&dir, b"not a gguf file at all".to_vec());
        let mut engine = GgufMetadataEngine::new();
        let error = engine
            .load_model(LoadModelRequest {
                model_id: "junk".into(),
                path,
                ..LoadModelRequest::default()
            })
            .expect_err("bad magic");
        assert!(matches!(error, AppError::GgufParse(_)));
    }

    #[test]
    fn catalog_metadata_fills_the_gaps() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_fixture(&dir, gguf_bytes(None, None));
        let mut engine = GgufMetadataEngine::new();
        let handle = engine
            .load_model(LoadModelRequest {
                model_id: "from-catalog".into(),
                path,
                metadata: Some(smollm_core::model::ModelMetadata {
                    architecture: Some("qwen2".into()),
                    quantization: Some("Q4_K_M".into()),
                    parameters_b: Some(0.5),
                    ..Default::default()
                }),
                ..LoadModelRequest::default()
            })
            .expect("loads");
        assert_eq!(handle.metadata.architecture.as_deref(), Some("qwen2"));
        assert_eq!(handle.metadata.quantization.as_deref(), Some("Q4_K_M"));
        assert_eq!(handle.metadata.parameters_b, Some(0.5));
        // The file's own name still wins over the catalog.
        assert_eq!(handle.metadata.name.as_deref(), Some("Fixture Model"));
    }

    #[test]
    fn unloading_clears_metrics() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_fixture(&dir, gguf_bytes(Some("qwen2"), None));
        let mut engine = GgufMetadataEngine::new();
        let handle = engine
            .load_model(LoadModelRequest {
                model_id: "x".into(),
                path,
                ..LoadModelRequest::default()
            })
            .expect("loads");
        assert!(engine.metrics().loaded);
        engine.unload_model(handle).expect("unloads");
        assert!(!engine.metrics().loaded);
        assert!(engine.metrics().model_id.is_none());
    }
}
