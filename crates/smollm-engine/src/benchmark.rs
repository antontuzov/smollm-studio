//! Benchmark runner: load time, prompt rate, generation rate, TTFT, peak RSS.
//!
//! The engine crate stays free of `sysinfo` here: callers inject a memory
//! sampler, which also makes the runner trivial to test.

use std::time::{Duration, Instant};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use smollm_core::chat::{approx_token_count, LoadModelRequest, SamplingParams};
use smollm_core::system::Backend;
use smollm_core::AppResult;

use crate::EngineManager;

/// What the user chooses on the Benchmarks page.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BenchmarkConfig {
    pub model_id: String,
    /// Rough prompt size to prefill, in tokens.
    pub prompt_tokens: u32,
    /// Tokens to generate.
    pub max_tokens: u32,
    pub context_length: u32,
    pub gpu_layers: i32,
    pub backend: Backend,
    /// Repeat the generation phase this many times and report the best run.
    pub runs: u32,
}

impl Default for BenchmarkConfig {
    fn default() -> Self {
        Self {
            model_id: String::new(),
            prompt_tokens: 128,
            max_tokens: 128,
            context_length: 4096,
            gpu_layers: -1,
            backend: Backend::default(),
            runs: 1,
        }
    }
}

impl BenchmarkConfig {
    /// Keep a benchmark inside what the app can actually ask an engine for.
    pub fn normalised(&self) -> Self {
        let mut out = self.clone();
        out.prompt_tokens = out.prompt_tokens.clamp(8, 4096);
        out.max_tokens = out.max_tokens.clamp(8, 2048);
        out.context_length = out
            .context_length
            .max(out.prompt_tokens + out.max_tokens + 16)
            .clamp(512, 32_768);
        out.runs = out.runs.clamp(1, 5);
        out
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.model_id.trim().is_empty() {
            return Err("choose a model to benchmark".to_string());
        }
        Ok(())
    }
}

/// Progress event emitted while a benchmark runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkProgress {
    pub stage: &'static str,
    pub percent: f64,
    pub message: String,
}

/// One measured generation pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RunMetrics {
    pub prompt_tokens: u32,
    pub generated_tokens: u32,
    pub time_to_first_token_ms: u64,
    pub generation_ms: u64,
    pub generation_tokens_per_second: f64,
    pub peak_rss_mb: Option<f64>,
}

/// Everything the Benchmarks table shows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BenchmarkResult {
    pub model_id: String,
    pub engine: String,
    /// False only when real weights ran. Mock results must be labelled.
    pub simulated: bool,
    pub backend: Backend,
    pub load_ms: u64,
    pub prompt_tokens_per_second: f64,
    pub generation_tokens_per_second: f64,
    pub time_to_first_token_ms: u64,
    pub peak_rss_mb: Option<f64>,
    pub prompt_tokens: u32,
    pub generated_tokens: u32,
    pub runs: u32,
    pub context_length: u32,
    pub warnings: Vec<String>,
}

impl BenchmarkResult {
    /// Markdown table matching the design spec's example.
    pub fn markdown_table(&self) -> String {
        let rss = self
            .peak_rss_mb
            .map(|mb| format!("{:.1} GB", mb / 1024.0))
            .unwrap_or_else(|| "n/a".to_string());
        format!(
            concat!(
                "| Metric | Value |\n",
                "|---|---|\n",
                "| Load time | {:.1} s |\n",
                "| Prompt tok/s | {:.0} |\n",
                "| Generation tok/s | {:.1} |\n",
                "| Time to first token | {} ms |\n",
                "| Peak RSS | {} |"
            ),
            self.load_ms as f64 / 1000.0,
            self.prompt_tokens_per_second,
            self.generation_tokens_per_second,
            self.time_to_first_token_ms,
            rss,
        )
    }
}

/// Prompt filler: a repeating sentence, trimmed to the requested token count.
const FILLER: &str = "The small model ran locally on the laptop while nothing left the machine. ";

pub fn build_prompt(target_tokens: u32) -> String {
    let target = target_tokens.max(1) as usize;
    let mut text = String::new();
    while approx_token_count(&text) < target as u32 {
        text.push_str(FILLER);
        // Guard against a runaway loop if the estimate ever stalls.
        if text.len() > target * 64 + 4096 {
            break;
        }
    }
    let clipped: String = text.chars().take(target * 4).collect();
    clipped
}

/// Run the benchmark against an already-constructed manager.
///
/// `sample_rss_mb` may return `None` when the platform cannot report resident
/// memory; results then simply omit that column.
pub async fn run<F>(
    manager: &mut EngineManager,
    config: &BenchmarkConfig,
    load_request: LoadModelRequest,
    sample_rss_mb: F,
    mut on_progress: impl FnMut(BenchmarkProgress),
) -> AppResult<BenchmarkResult>
where
    F: Fn() -> Option<f64>,
{
    let config = config.normalised();
    let rss_before = sample_rss_mb();

    on_progress(BenchmarkProgress {
        stage: "loading",
        percent: 0.0,
        message: format!("loading {}", config.model_id),
    });
    let load_started = Instant::now();
    let loaded = manager.load(load_request)?;
    let load_ms = elapsed_ms(load_started);
    on_progress(BenchmarkProgress {
        stage: "loading",
        percent: 25.0,
        message: format!("loaded in {} ms", load_ms),
    });

    let prompt = build_prompt(config.prompt_tokens);
    let prompt_tokens = approx_token_count(&prompt);
    let mut warnings = Vec::new();
    if loaded.simulated {
        warnings.push(format!(
            "{} engine is simulated: these numbers describe the mock, not real inference",
            loaded.engine
        ));
    }
    if prompt_tokens < config.prompt_tokens {
        warnings.push(format!(
            "prompt reached {prompt_tokens} tokens of the requested {}",
            config.prompt_tokens
        ));
    }

    let params = SamplingParams {
        max_tokens: config.max_tokens,
        ..SamplingParams::default()
    };

    let mut best: Option<RunMetrics> = None;
    for index in 0..config.runs {
        on_progress(BenchmarkProgress {
            stage: "generating",
            percent: 25.0 + 70.0 * f64::from(index) / f64::from(config.runs),
            message: format!("run {}/{}, generating", index + 1, config.runs),
        });
        let metrics = time_generation(manager, &prompt, &params, index, &sample_rss_mb).await?;
        let better = match &best {
            None => true,
            Some(current) => {
                metrics.generation_tokens_per_second > current.generation_tokens_per_second
            }
        };
        if better {
            best = Some(metrics);
        }
    }

    let metrics = best.unwrap_or_default();
    on_progress(BenchmarkProgress {
        stage: "done",
        percent: 100.0,
        message: "benchmark complete".to_string(),
    });

    let peak_rss_mb = max_rss(rss_before, metrics.peak_rss_mb);
    if peak_rss_mb.is_none() {
        warnings.push("resident memory could not be sampled on this platform".to_string());
    }

    let simulated = manager.is_simulated();
    let prefill_rate = prompt_rate(prompt_tokens, metrics.time_to_first_token_ms);
    if simulated {
        // A mock has no real prefill phase, so the rate is a modelled estimate.
        warnings.push("prompt tok/s is derived from simulated timings".to_string());
    }

    Ok(BenchmarkResult {
        model_id: config.model_id.clone(),
        engine: manager.engine_name(),
        simulated,
        backend: config.backend,
        load_ms,
        prompt_tokens_per_second: prefill_rate,
        generation_tokens_per_second: metrics.generation_tokens_per_second,
        time_to_first_token_ms: metrics.time_to_first_token_ms,
        peak_rss_mb,
        prompt_tokens,
        generated_tokens: metrics.generated_tokens,
        runs: config.runs,
        context_length: loaded.handle.context_length,
        warnings,
    })
}

async fn time_generation<F>(
    manager: &mut EngineManager,
    prompt: &str,
    params: &SamplingParams,
    attempt: u32,
    sample_rss_mb: &F,
) -> AppResult<RunMetrics>
where
    F: Fn() -> Option<f64>,
{
    let request_id = format!("bench-{}-{attempt}", uuid::Uuid::new_v4().simple());
    let mut stream =
        manager.start_generation(request_id, prompt.to_string(), params.clone(), Vec::new())?;

    let started = Instant::now();
    let mut first_token_ms = None;
    let mut generated = 0u32;
    let mut peak: Option<f64> = None;

    while let Some(item) = stream.next().await {
        let token = item?;
        if token.finish_reason.is_some() {
            if let Some(usage) = token.usage {
                generated = generated.max(usage.completion_tokens);
            }
            continue;
        }
        if token.text.is_empty() {
            continue;
        }
        if first_token_ms.is_none() {
            first_token_ms = Some(elapsed_ms(started));
        }
        generated += 1;
        if generated % 16 == 0 {
            if let Some(rss) = sample_rss_mb() {
                peak = Some(peak.map_or(rss, |current: f64| current.max(rss)));
            }
        }
    }

    let generation_ms = elapsed_ms(started);
    let rate = if generation_ms > 0 && generated > 0 {
        f64::from(generated) * 1000.0 / generation_ms as f64
    } else {
        0.0
    };

    Ok(RunMetrics {
        prompt_tokens: approx_token_count(prompt),
        generated_tokens: generated,
        time_to_first_token_ms: first_token_ms.unwrap_or(generation_ms),
        generation_ms,
        generation_tokens_per_second: rate,
        peak_rss_mb: peak,
    })
}

/// Prefill rate: prompt tokens divided by the time to the first token.
fn prompt_rate(prompt_tokens: u32, ttft_ms: u64) -> f64 {
    if prompt_tokens == 0 || ttft_ms == 0 {
        return 0.0;
    }
    f64::from(prompt_tokens) * 1000.0 / ttft_ms as f64
}

fn max_rss(before: Option<f64>, during: Option<f64>) -> Option<f64> {
    match (before, during) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    let elapsed: Duration = start.elapsed();
    elapsed.as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    // The mock engine is optional, so the tests that need it are too; the rest of
    // this module is feature-independent and always runs.
    #[cfg(feature = "mock")]
    use crate::mock::{MockConfig, MockEngine};

    fn spec(model_id: &str) -> BenchmarkConfig {
        BenchmarkConfig {
            model_id: model_id.to_string(),
            prompt_tokens: 32,
            max_tokens: 24,
            ..BenchmarkConfig::default()
        }
    }

    fn request(model_id: &str) -> LoadModelRequest {
        LoadModelRequest {
            model_id: model_id.to_string(),
            ..LoadModelRequest::default()
        }
    }

    #[test]
    fn prompts_are_built_to_the_requested_size() {
        assert!(build_prompt(1).is_empty() || approx_token_count(&build_prompt(1)) >= 1);
        let prompt = build_prompt(64);
        let count = approx_token_count(&prompt);
        assert!(
            (48..=80).contains(&count),
            "expected ~64 tokens, got {count}"
        );
        assert!(build_prompt(8).len() < build_prompt(256).len());
    }

    #[test]
    fn configs_are_normalised_into_safe_bounds() {
        let wild = BenchmarkConfig {
            model_id: " x ".to_string(),
            prompt_tokens: 0,
            max_tokens: 999_999,
            context_length: 1,
            runs: 50,
            ..BenchmarkConfig::default()
        };
        let calm = wild.normalised();
        assert_eq!(calm.prompt_tokens, 8);
        assert_eq!(calm.max_tokens, 2048);
        assert!(calm.context_length >= 8 + 2048 + 16);
        assert!(calm.context_length <= 32_768);
        assert_eq!(calm.runs, 5);
        assert!(calm.validate().is_ok());

        let missing = BenchmarkConfig {
            model_id: "   ".to_string(),
            ..BenchmarkConfig::default()
        };
        assert!(missing.validate().is_err());
    }

    #[cfg(feature = "mock")]
    #[tokio::test]
    async fn benchmarks_run_end_to_end_against_the_mock() {
        let mut manager =
            EngineManager::with_engine(Box::new(MockEngine::with_config(MockConfig::instant())));
        let mut stages = Vec::new();
        let result = run(
            &mut manager,
            &spec("qwen2.5-0.5b-instruct-gguf"),
            request("qwen2.5-0.5b-instruct-gguf"),
            || Some(1024.0),
            |progress| stages.push(progress.stage),
        )
        .await
        .expect("benchmarks");

        assert_eq!(result.model_id, "qwen2.5-0.5b-instruct-gguf");
        assert_eq!(result.engine, "mock");
        assert!(result.simulated, "mock results must be labelled");
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("simulated")));
        assert!(result.generated_tokens > 0);
        assert!(result.load_ms < 5_000);
        assert_eq!(result.runs, 1);
        assert_eq!(result.context_length, 4096);
        assert_eq!(result.peak_rss_mb, Some(1024.0));
        assert_eq!(stages, vec!["loading", "loading", "generating", "done"]);
        assert!(result.markdown_table().contains("| Peak RSS | 1.0 GB |"));
    }

    #[cfg(feature = "mock")]
    #[tokio::test]
    async fn repeated_runs_report_the_best_pass() {
        let mut manager =
            EngineManager::with_engine(Box::new(MockEngine::with_config(MockConfig::instant())));
        let config = BenchmarkConfig {
            runs: 3,
            ..spec("smollm2-135m-gguf")
        };
        let result = run(
            &mut manager,
            &config,
            request("smollm2-135m-gguf"),
            || None,
            |_| {},
        )
        .await
        .expect("benchmarks");
        assert_eq!(result.runs, 3);
        assert!(result.peak_rss_mb.is_none());
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("resident memory")));
    }

    #[tokio::test]
    async fn an_unusable_engine_surfaces_its_error() {
        // The metadata engine cannot generate, so a benchmark must fail cleanly.
        let mut manager = EngineManager::with_kind(crate::EngineKind::GgufMetadata);
        let error = run(
            &mut manager,
            &spec("nope.gguf"),
            request("nope.gguf"),
            || None,
            |_| {},
        )
        .await
        .expect_err("loading without a file fails");
        assert!(matches!(
            error,
            smollm_core::AppError::ModelNotDownloaded(_)
        ));
    }

    #[test]
    fn rates_are_derived_from_measured_time() {
        assert_eq!(prompt_rate(0, 100), 0.0);
        assert_eq!(prompt_rate(100, 0), 0.0);
        assert_eq!(prompt_rate(100, 50), 2000.0);
        assert_eq!(max_rss(Some(10.0), Some(9.0)), Some(10.0));
        assert_eq!(max_rss(None, Some(4.0)), Some(4.0));
        assert_eq!(max_rss(None, None), None);
    }

    #[test]
    fn markdown_matches_the_documented_table() {
        let result = BenchmarkResult {
            model_id: "m".to_string(),
            engine: "mock".to_string(),
            simulated: true,
            backend: Backend::Cpu,
            load_ms: 1800,
            prompt_tokens_per_second: 412.0,
            generation_tokens_per_second: 38.0,
            time_to_first_token_ms: 220,
            peak_rss_mb: Some(2150.0),
            prompt_tokens: 128,
            generated_tokens: 128,
            runs: 1,
            context_length: 4096,
            warnings: Vec::new(),
        };
        let table = result.markdown_table();
        assert!(table.contains("| Load time | 1.8 s |"), "{table}");
        assert!(table.contains("| Prompt tok/s | 412 |"));
        assert!(table.contains("| Generation tok/s | 38.0 |"));
        assert!(table.contains("| Time to first token | 220 ms |"));
        assert!(table.contains("| Peak RSS | 2.1 GB |"));

        let no_rss = BenchmarkResult {
            peak_rss_mb: None,
            ..result
        };
        assert!(no_rss.markdown_table().contains("| Peak RSS | n/a |"));
    }
}
