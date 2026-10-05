//! Curated small-model catalog: loading, filtering, validation and
//! hardware-aware recommendations.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use smollm_core::error::{AppError, AppResult};
use smollm_core::model::{CatalogStatus, ModelDescriptor, Rating};
use smollm_core::paths::AppPaths;
use smollm_core::system::DoctorReport;

/// Embedded curated catalog; checked by `validate()` in the tests below.
pub const EMBEDDED_CATALOG_JSON: &str = include_str!("../data/catalog.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogFile {
    schema_version: u32,
    #[serde(default)]
    models: Vec<ModelDescriptor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    /// Quality/speed weighted by how well the model fits the machine.
    #[default]
    Recommended,
    Smallest,
    Largest,
    Fastest,
    Name,
}

/// Search/filter controls from the Models page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CatalogFilters {
    pub query: String,
    pub max_parameters_b: Option<f32>,
    pub min_parameters_b: Option<f32>,
    pub quantization: Option<String>,
    pub tag: Option<String>,
    pub license: Option<String>,
    /// GGUF architecture, e.g. `llama` or `qwen2`.
    pub architecture: Option<String>,
    /// Hide entries whose Hugging Face coordinates are unconfirmed.
    pub hide_placeholders: bool,
    pub sort: SortKey,
}

/// One selectable value in a filter dropdown, with how many entries offer it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetValue {
    pub value: String,
    pub count: usize,
}

/// Everything the Models page needs to build its filter controls, derived from
/// the loaded catalog so a `catalog.local.json` overlay extends the options too.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogFacets {
    pub quantizations: Vec<FacetValue>,
    pub tags: Vec<FacetValue>,
    pub licenses: Vec<FacetValue>,
    pub architectures: Vec<FacetValue>,
    /// Largest `parametersB` in the catalog; the UI builds its size bands from
    /// this rather than hardcoding a ceiling.
    pub max_parameters_b: f32,
}

fn counted(values: impl Iterator<Item = String>) -> Vec<FacetValue> {
    let mut tally: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for value in values {
        *tally.entry(value).or_default() += 1;
    }
    let mut facets: Vec<FacetValue> = tally
        .into_iter()
        .map(|(value, count)| FacetValue { value, count })
        .collect();
    // Most useful first, alphabetically when counts tie.
    facets.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
    facets
}

#[derive(Debug, Clone)]
pub struct ModelCatalog {
    models: Vec<ModelDescriptor>,
    /// Non-fatal problems found while loading (surfaced in Settings > diagnostics).
    pub warnings: Vec<String>,
}

impl ModelCatalog {
    pub fn parse(text: &str) -> AppResult<Self> {
        let file: CatalogFile = serde_json::from_str(text)?;
        if file.schema_version != 1 {
            return Err(AppError::Config(format!(
                "unsupported catalog schema {} (expected 1)",
                file.schema_version
            )));
        }
        Ok(Self {
            models: file.models,
            warnings: Vec::new(),
        })
    }

    /// The bundled catalog. Falls back to an empty catalog instead of panicking
    /// so a malformed override can never stop the app from starting.
    pub fn embedded() -> Self {
        Self::parse(EMBEDDED_CATALOG_JSON).unwrap_or_else(|error| Self {
            models: Vec::new(),
            warnings: vec![format!("embedded catalog failed to parse: {error}")],
        })
    }

    /// Bundled catalog plus an optional `catalog.local.json` overlay in the data
    /// directory. Overlay entries replace same-id entries and may add private models.
    pub fn load(paths: &AppPaths) -> Self {
        let mut catalog = Self::embedded();
        let overlay = paths.data_dir.join("catalog.local.json");
        if overlay.exists() {
            match std::fs::read_to_string(&overlay)
                .map_err(AppError::from)
                .and_then(|text| Self::parse(&text))
            {
                Ok(added) => {
                    for model in added.models {
                        match catalog
                            .models
                            .iter_mut()
                            .find(|existing| existing.id == model.id)
                        {
                            Some(existing) => *existing = model,
                            None => catalog.models.push(model),
                        }
                    }
                    catalog.warnings.extend(added.warnings);
                }
                Err(error) => catalog
                    .warnings
                    .push(format!("ignored {}: {error}", overlay.display())),
            }
        }
        catalog.sort_recommended();
        catalog
    }

    pub fn models(&self) -> &[ModelDescriptor] {
        &self.models
    }

    pub fn find(&self, id: &str) -> Option<&ModelDescriptor> {
        self.models.iter().find(|model| model.id == id)
    }

    /// Find a catalog entry from a local filename (used by the Library page).
    pub fn find_by_filename(&self, file_name: &str) -> Option<&ModelDescriptor> {
        self.models
            .iter()
            .find(|model| model.filename.eq_ignore_ascii_case(file_name))
    }

    pub fn small_models(&self) -> Vec<&ModelDescriptor> {
        self.models
            .iter()
            .filter(|model| model.is_small())
            .collect()
    }

    /// Distinct values for the Models page filters, with entry counts.
    pub fn facets(&self) -> CatalogFacets {
        CatalogFacets {
            quantizations: counted(self.models.iter().map(|model| model.quantization.clone())),
            tags: counted(self.models.iter().flat_map(|model| model.tags.clone())),
            licenses: counted(self.models.iter().map(|model| model.license.clone())),
            architectures: counted(self.models.iter().map(|model| model.architecture.clone())),
            max_parameters_b: self
                .models
                .iter()
                .map(|model| model.parameters_b)
                .fold(0.0f32, f32::max),
        }
    }

    pub fn filter(&self, filters: &CatalogFilters) -> Vec<ModelDescriptor> {
        let query = filters.query.trim().to_lowercase();
        let mut results: Vec<ModelDescriptor> = self
            .models
            .iter()
            .filter(|model| {
                if filters.hide_placeholders && model.status == CatalogStatus::Placeholder {
                    return false;
                }
                if let Some(max) = filters.max_parameters_b {
                    if model.parameters_b > max {
                        return false;
                    }
                }
                if let Some(min) = filters.min_parameters_b {
                    if model.parameters_b < min {
                        return false;
                    }
                }
                if let Some(quantization) = filters.quantization.as_deref() {
                    if !model.quantization.eq_ignore_ascii_case(quantization) {
                        return false;
                    }
                }
                if let Some(tag) = filters.tag.as_deref() {
                    if !model
                        .tags
                        .iter()
                        .any(|candidate| candidate.eq_ignore_ascii_case(tag))
                    {
                        return false;
                    }
                }
                if let Some(license) = filters.license.as_deref() {
                    if !model.license.eq_ignore_ascii_case(license) {
                        return false;
                    }
                }
                if let Some(architecture) = filters.architecture.as_deref() {
                    if !model.architecture.eq_ignore_ascii_case(architecture) {
                        return false;
                    }
                }
                if query.is_empty() {
                    return true;
                }
                model.display_name.to_lowercase().contains(&query)
                    || model.id.to_lowercase().contains(&query)
                    || model.hf_repo.to_lowercase().contains(&query)
                    || model
                        .tags
                        .iter()
                        .any(|tag| tag.to_lowercase().contains(&query))
            })
            .cloned()
            .collect();

        match filters.sort {
            SortKey::Smallest => results.sort_by(|a, b| a.parameters_b.total_cmp(&b.parameters_b)),
            SortKey::Largest => results.sort_by(|a, b| b.parameters_b.total_cmp(&a.parameters_b)),
            SortKey::Fastest => results.sort_by_key(|model| std::cmp::Reverse(speed_rank(model))),
            SortKey::Name => results.sort_by(|a, b| a.display_name.cmp(&b.display_name)),
            SortKey::Recommended => {
                results.sort_by(|a, b| a.parameters_b.total_cmp(&b.parameters_b))
            }
        }
        results
    }

    /// Models this machine can run comfortably, smallest first.
    pub fn recommended_for(&self, doctor: &DoctorReport, limit: usize) -> Vec<ModelDescriptor> {
        let available = doctor.hardware.available_ram_gb.max(0.0);
        let disk = doctor.hardware.model_volume_free_gb.max(0.0);
        let mut matches: Vec<ModelDescriptor> = self
            .models
            .iter()
            .filter(|model| model.status != CatalogStatus::Placeholder)
            .filter(|model| model.parameters_b <= doctor.max_comfortable_parameters_b + 0.01)
            .filter(|model| {
                model.estimated_ram_gb(model.context_length.min(8192)) <= available * 0.7
            })
            .filter(|model| (model.size_mb as f64) / 1000.0 <= disk * 0.5)
            .cloned()
            .collect();
        matches.sort_by(|a, b| {
            score(b)
                .partial_cmp(&score(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        matches.truncate(limit);
        matches
    }

    fn sort_recommended(&mut self) {
        self.models
            .sort_by(|a, b| a.parameters_b.total_cmp(&b.parameters_b));
    }

    /// Structural validation used by tests and Settings > diagnostics.
    pub fn validate(&self) -> AppResult<Vec<String>> {
        let mut seen = HashSet::new();
        let mut problems = Vec::new();

        for model in &self.models {
            if model.id.trim().is_empty() {
                problems.push("catalog entry has an empty id".to_string());
                continue;
            }
            if !seen.insert(model.id.clone()) {
                problems.push(format!("duplicate catalog id {}", model.id));
            }
            if !model.hf_repo.contains('/') {
                problems.push(format!("{}: hfRepo must be `org/name`", model.id));
            }
            if !model.filename.to_lowercase().ends_with(".gguf") {
                problems.push(format!("{}: filename must end in .gguf", model.id));
            }
            if model.revision.trim().is_empty() {
                problems.push(format!("{}: revision must not be empty", model.id));
            }
            if !(0.1f32..=200.0f32).contains(&model.parameters_b) {
                problems.push(format!("{}: parametersB out of range", model.id));
            }
            if model.size_mb == 0 || model.size_mb > 500_000 {
                problems.push(format!(
                    "{}: implausible sizeMb {}",
                    model.id, model.size_mb
                ));
            }
            if model.context_length < 512 {
                problems.push(format!("{}: contextLength too small", model.id));
            }
            if model.description.trim().is_empty() {
                problems.push(format!("{}: missing description", model.id));
            }
            if model.architecture.trim().is_empty() {
                problems.push(format!("{}: missing architecture", model.id));
            }
            let url = model.download_url();
            if !url.starts_with("https://huggingface.co/") || url.contains(" ") {
                problems.push(format!("{}: bad download URL {url}", model.id));
            }
        }

        Ok(problems)
    }
}

fn speed_rank(model: &ModelDescriptor) -> u8 {
    match model.speed {
        Rating::Excellent => 4,
        Rating::Good => 3,
        Rating::Fair => 2,
        Rating::Poor => 1,
    }
}

fn score(model: &ModelDescriptor) -> f32 {
    // Small models are only worth recommending if the quality is there; this
    // keeps 360M below 1.5B while still listing it as a low-RAM option.
    let quality = match model.quality {
        Rating::Excellent => 3.0,
        Rating::Good => 2.0,
        Rating::Fair => 1.0,
        Rating::Poor => 0.0,
    };
    let speed = f32::from(speed_rank(model));
    quality * 2.0 + speed - model.parameters_b * 0.2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholder_catalog() -> ModelCatalog {
        ModelCatalog::parse(
            r#"{
              "schemaVersion": 1,
              "models": [{
                "id": "example-placeholder",
                "displayName": "Example Placeholder",
                "hfRepo": "example/not-real",
                "filename": "example-q4_k_m.gguf",
                "parametersB": 900,
                "quantization": "Q4_K_M",
                "sizeMb": 0,
                "contextLength": 128,
                "license": "unknown",
                "family": "other",
                "tags": ["chat"],
                "status": "placeholder",
                "description": "Deliberately invalid entry."
              }]
            }"#,
        )
        .expect("parses")
    }

    #[test]
    fn rejects_unknown_schema() {
        let error = ModelCatalog::parse(r#"{"schemaVersion": 7, "models": []}"#)
            .expect_err("schema checked");
        assert!(error.to_string().contains("unsupported catalog schema"));
    }

    #[test]
    fn embedded_catalog_is_valid() {
        let catalog = ModelCatalog::embedded();
        assert!(!catalog.models().is_empty());
        let problems = catalog.validate().expect("validation runs");
        assert_eq!(
            problems,
            Vec::<String>::new(),
            "catalog problems: {problems:?}"
        );
    }

    #[test]
    fn embedded_catalog_covers_the_small_model_bands() {
        let catalog = ModelCatalog::embedded();
        // The same buckets the Models page offers as size bands.
        for (label, min, max) in [
            ("under 1B", 0.0f32, 1.0),
            ("1-2B", 1.0, 2.0),
            ("2-3B", 2.0, 3.0),
            ("3-4B", 3.0, 4.0),
            ("4B and up", 4.0, 100.0),
        ] {
            assert!(
                catalog
                    .models()
                    .iter()
                    .any(|model| model.parameters_b >= min && model.parameters_b < max),
                "no catalog entry in the {label} band"
            );
        }
        assert!(catalog.models().iter().all(ModelDescriptor::is_small));
    }

    #[test]
    fn validation_reports_bad_entries() {
        let catalog = placeholder_catalog();
        let problems = catalog.validate().expect("validation runs");
        assert!(problems
            .iter()
            .any(|problem| problem.contains("parametersB")));
        assert!(problems.iter().any(|problem| problem.contains("sizeMb")));
        assert!(problems
            .iter()
            .any(|problem| problem.contains("contextLength")));
        assert!(problems
            .iter()
            .any(|problem| problem.contains("missing architecture")));
    }

    #[test]
    fn filters_by_architecture_license_and_size_band() {
        let catalog = ModelCatalog::embedded();

        let qwen = catalog.filter(&CatalogFilters {
            architecture: Some("qwen2".to_string()),
            ..Default::default()
        });
        assert!(!qwen.is_empty());
        assert!(qwen.iter().all(|model| model.architecture == "qwen2"));

        let apache = catalog.filter(&CatalogFilters {
            license: Some("apache-2.0".to_string()),
            ..Default::default()
        });
        assert!(!apache.is_empty());
        assert!(apache.iter().all(|model| model.license == "apache-2.0"));

        let band = catalog.filter(&CatalogFilters {
            min_parameters_b: Some(2.0),
            max_parameters_b: Some(2.99),
            ..Default::default()
        });
        assert!(!band.is_empty(), "the 2B band needs at least one entry");
        assert!(band
            .iter()
            .all(|model| model.parameters_b >= 2.0 && model.parameters_b < 3.0));

        // Filters compose: an empty intersection is a real answer, not a bug.
        let nonsense = catalog.filter(&CatalogFilters {
            architecture: Some("qwen2".to_string()),
            license: Some("llama3.2".to_string()),
            ..Default::default()
        });
        assert!(nonsense.is_empty());
    }

    #[test]
    fn facets_list_every_filterable_value_with_counts() {
        let catalog = ModelCatalog::embedded();
        let facets = catalog.facets();

        assert_eq!(
            facets
                .architectures
                .iter()
                .map(|facet| facet.count)
                .sum::<usize>(),
            catalog.models().len()
        );
        assert!(facets
            .architectures
            .iter()
            .any(|facet| facet.value == "granite"));
        assert!(facets
            .quantizations
            .iter()
            .any(|facet| facet.value == "Q4_K_M"));
        assert!(facets.tags.iter().any(|facet| facet.value == "reasoning"));
        assert!(!facets.licenses.is_empty());
        // Sorted by count descending, so the widest options come first.
        assert!(facets
            .tags
            .windows(2)
            .all(|pair| pair[0].count >= pair[1].count));
        assert!(facets.max_parameters_b >= 4.0);
    }

    #[test]
    fn filters_search_and_hide_placeholders() {
        let catalog = ModelCatalog::embedded();

        let by_query = catalog.filter(&CatalogFilters {
            query: "smollm".to_string(),
            ..Default::default()
        });
        assert!(!by_query.is_empty());
        assert!(by_query.iter().all(|model| model.id.contains("smollm")));

        let by_tag = catalog.filter(&CatalogFilters {
            tag: Some("reasoning".to_string()),
            ..Default::default()
        });
        assert!(by_tag.iter().any(|model| model.id.contains("deepseek")));

        let capped = catalog.filter(&CatalogFilters {
            max_parameters_b: Some(1.0),
            ..Default::default()
        });
        assert!(!capped.is_empty());
        assert!(capped.iter().all(|model| model.parameters_b <= 1.0));

        let hidden = catalog.filter(&CatalogFilters {
            hide_placeholders: true,
            query: "example-placeholder".to_string(),
            ..Default::default()
        });
        assert!(hidden.is_empty());
        let shown = placeholder_catalog().filter(&CatalogFilters {
            hide_placeholders: false,
            ..Default::default()
        });
        assert_eq!(shown.len(), 1);
    }

    #[test]
    fn sorting_orders_by_size_and_name() {
        let catalog = ModelCatalog::embedded();
        let smallest_first = catalog.filter(&CatalogFilters {
            sort: SortKey::Smallest,
            ..Default::default()
        });
        assert!(smallest_first
            .windows(2)
            .all(|pair| pair[0].parameters_b <= pair[1].parameters_b));

        let by_name = catalog.filter(&CatalogFilters {
            sort: SortKey::Name,
            ..Default::default()
        });
        assert!(by_name
            .windows(2)
            .all(|pair| pair[0].display_name <= pair[1].display_name));

        let fastest = catalog.filter(&CatalogFilters {
            sort: SortKey::Fastest,
            ..Default::default()
        });
        assert_eq!(fastest.len(), catalog.models().len());
    }

    #[test]
    fn recommendations_respect_a_tiny_machine() {
        let catalog = ModelCatalog::embedded();
        let doctor = doctor_with(0.5, 2.0, 500.0);
        let picks = catalog.recommended_for(&doctor, 5);
        assert!(
            picks.iter().all(|model| model.parameters_b <= 0.51),
            "got {:?}",
            picks.iter().map(|m| m.id.as_str()).collect::<Vec<_>>()
        );

        let roomy = catalog.recommended_for(&doctor_with(4.0, 16.0, 200.0), 10);
        assert!(roomy.iter().any(|model| model.parameters_b >= 1.5));

        // Almost no free disk leaves nothing to recommend.
        let cramped_disk = catalog.recommended_for(&doctor_with(4.0, 16.0, 0.4), 10);
        assert!(cramped_disk.is_empty());
    }

    #[test]
    fn lookup_by_id_and_filename() {
        let catalog = ModelCatalog::embedded();
        assert!(catalog.find("qwen2.5-0.5b-instruct-gguf").is_some());
        assert!(catalog.find("nope").is_none());
        let model = catalog
            .find("qwen2.5-0.5b-instruct-gguf")
            .expect("curated id exists");
        assert_eq!(
            catalog
                .find_by_filename(&model.filename.to_lowercase())
                .map(|m| m.id.clone()),
            Some("qwen2.5-0.5b-instruct-gguf".to_string())
        );
        assert!(catalog.small_models().len() >= 5);
    }

    fn doctor_with(max_params: f32, available_ram_gb: f64, disk_free_gb: f64) -> DoctorReport {
        DoctorReport {
            platform: "macos".to_string(),
            arch: "aarch64".to_string(),
            cpu_cores: 10,
            ram_gb: available_ram_gb,
            gpu: "Apple M3".to_string(),
            backend_recommendation: smollm_core::system::Backend::Metal,
            recommended_models: Vec::new(),
            warnings: Vec::new(),
            headline: String::new(),
            detail: String::new(),
            max_comfortable_parameters_b: max_params,
            hardware: smollm_core::system::HardwareReport {
                available_ram_gb,
                model_volume_free_gb: disk_free_gb,
                ..Default::default()
            },
        }
    }
}
