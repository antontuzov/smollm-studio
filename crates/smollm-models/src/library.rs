//! Local model library: scan, import, verify and delete downloaded GGUF files.

use std::path::{Path, PathBuf};

use smollm_core::error::{AppError, AppResult};
use smollm_core::gguf::GgufHeader;
use smollm_core::model::{
    LocalModel, ModelDescriptor, ModelMetadata, ModelVerification, VerificationCheck,
};
use smollm_core::paths::AppPaths;

use crate::catalog::ModelCatalog;
use crate::download::{free_space, gb, partial_path_for, sanitize_file_name, size_matches};

/// Weight bytes per parameter that any real quantisation could produce. Below
/// Q2_0 there are fewer bytes than a compressed tensor holds; above a relaxed
/// f32 the parameter count or the data section is nonsense.
const MIN_BYTES_PER_PARAM: f64 = 0.3;
const MAX_BYTES_PER_PARAM: f64 = 8.0;

/// A directory of `.gguf` files plus catalog knowledge about them.
#[derive(Debug, Clone)]
pub struct ModelLibrary {
    paths: AppPaths,
}

impl ModelLibrary {
    pub fn new(paths: AppPaths) -> Self {
        Self { paths }
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// List every local GGUF, richest metadata first.
    pub fn scan(&self, catalog: &ModelCatalog) -> AppResult<Vec<LocalModel>> {
        self.paths.ensure()?;
        let mut models = Vec::new();

        let entries = walkdir::WalkDir::new(&self.paths.models_dir)
            .max_depth(3)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok);

        for entry in entries {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("gguf") {
                // Skips `.gguf.part` resume files and anything unrelated.
                continue;
            }
            match Self::to_local_model(path, catalog) {
                Ok(model) => models.push(model),
                Err(error) => {
                    tracing::warn!(path = %path.display(), "ignoring unreadable model file: {error}")
                }
            }
        }

        models.sort_by(|a, b| {
            a.display_name()
                .to_lowercase()
                .cmp(&b.display_name().to_lowercase())
        });
        Ok(models)
    }

    /// One file, fully parsed. Used by the Library detail view.
    pub fn inspect(&self, path: &Path, catalog: &ModelCatalog) -> AppResult<LocalModel> {
        Self::to_local_model(path, catalog)
    }

    /// Bring a GGUF the user already has into the model folder, and list it.
    ///
    /// Copied, never moved: an import the user later regrets should not have cost
    /// them the only copy they had. The header is parsed from the source before
    /// anything is written, because the alternative is discovering a wrong
    /// extension after copying several gigabytes of it. The copy lands on a
    /// `.part` name and is renamed at the end, exactly like a download, so an
    /// interrupted import is swept up on the next start instead of appearing in
    /// the library as a model.
    ///
    /// A file that already lives in the model folder is listed without a second
    /// copy, which is what a dragged-back library file should do.
    pub fn import(&self, source: &Path, catalog: &ModelCatalog) -> AppResult<LocalModel> {
        self.paths.ensure()?;
        let source = checked_gguf(source)?;
        let size = file_size(&source)?;

        let models_dir = self
            .paths
            .models_dir
            .canonicalize()
            .unwrap_or_else(|_| self.paths.models_dir.clone());
        let base = sanitize_file_name(
            source
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
        )?;
        let file_name = numerate(&base, &models_dir, &source)?;
        let dest = models_dir.join(&file_name);
        if dest == source {
            tracing::info!(file = %file_name, "model is already in the library");
            return Self::to_local_model(&dest, catalog);
        }

        let free = free_space(&models_dir)?;
        if size > free {
            return Err(AppError::InsufficientDiskSpace {
                required_gb: gb(size),
                available_gb: gb(free),
            });
        }

        let temp = partial_path_for(&dest);
        write_copy(&source, &temp, size).inspect_err(|_| {
            // A failed copy must not leave a file the library will one day scan.
            let _ = std::fs::remove_file(&temp);
        })?;
        std::fs::rename(&temp, &dest)?;

        tracing::info!(source = %source.display(), file = %file_name, bytes = size, "imported a local model");
        Self::to_local_model(&dest, catalog)
    }

    /// Resolve a catalog id (or bare filename) to a local file.
    pub fn resolve(&self, catalog: &ModelCatalog, model_id: &str) -> AppResult<PathBuf> {
        if let Some(model) = catalog.find(model_id) {
            let path = self.paths.models_dir.join(&model.filename);
            if path.exists() {
                return Ok(path);
            }
            return Err(AppError::ModelNotDownloaded(model.id.clone()));
        }

        // Not in the catalog: accept a filename that lives in the model dir.
        let file_name = sanitize_file_name(model_id)?;
        let path = self.paths.models_dir.join(file_name);
        if path.exists() {
            return Ok(path);
        }
        Err(AppError::ModelNotFound(model_id.to_string()))
    }

    /// Re-check one library file: is it whole, is it a model, is it the size the
    /// catalog published? See [`verify_path`] for what a header can and cannot
    /// prove.
    pub fn verify(&self, file_name: &str, catalog: &ModelCatalog) -> AppResult<ModelVerification> {
        let path = self.library_file(file_name)?;
        Ok(verify_path(&path, catalog))
    }

    /// Re-check everything the scan lists, in list order.
    ///
    /// Built on the scan rather than on filenames, so a file in a subfolder of
    /// the model directory is verified at the path it was found at.
    pub fn verify_all(&self, catalog: &ModelCatalog) -> AppResult<Vec<ModelVerification>> {
        let models = self.scan(catalog)?;
        Ok(models
            .iter()
            .map(|model| verify_path(Path::new(&model.path), catalog))
            .collect())
    }

    pub fn exists(&self, model: &ModelDescriptor) -> bool {
        self.paths.models_dir.join(&model.filename).exists()
    }

    pub fn local_path(&self, model: &ModelDescriptor) -> PathBuf {
        self.paths.models_dir.join(&model.filename)
    }

    /// Delete a local file. Only files inside the model directory can be removed.
    pub fn delete(&self, file_name: &str) -> AppResult<PathBuf> {
        let path = self.library_file(file_name)?;
        std::fs::remove_file(&path)?;
        tracing::info!(path = %path.display(), "deleted local model");
        Ok(path)
    }

    /// A file inside the model directory, resolved and guarded against traversal.
    fn library_file(&self, file_name: &str) -> AppResult<PathBuf> {
        let sanitized = sanitize_file_name(file_name)?;
        let path = self.paths.models_dir.join(&sanitized);
        let canonical = path
            .canonicalize()
            .map_err(|_| AppError::ModelNotFound(format!("no local model named {sanitized}")))?;
        let root = self
            .paths
            .models_dir
            .canonicalize()
            .unwrap_or_else(|_| self.paths.models_dir.clone());
        if !canonical.starts_with(&root) {
            return Err(AppError::InvalidRequest(format!(
                "refusing to touch a file outside the model directory: {}",
                canonical.display()
            )));
        }
        if canonical.is_dir() {
            return Err(AppError::InvalidRequest(
                "model path is a directory, not a file".into(),
            ));
        }
        Ok(canonical)
    }

    pub fn total_size_bytes(&self, models: &[LocalModel]) -> u64 {
        models.iter().map(|model| model.size_bytes).sum()
    }

    pub fn free_space_bytes(&self) -> AppResult<u64> {
        free_space(&self.paths.models_dir)
    }

    fn to_local_model(path: &Path, catalog: &ModelCatalog) -> AppResult<LocalModel> {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let stat = std::fs::metadata(path)?;
        let modified_ms = stat
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_millis() as u64)
            .unwrap_or_default();

        let catalog_match = catalog.find_by_filename(&file_name);
        let (metadata, parse_error) = match GgufHeader::read(path) {
            Ok(header) => (merge(header.summary(), catalog_match), None),
            Err(error) => (
                metadata_from_catalog(catalog_match, &file_name),
                Some(error.to_string()),
            ),
        };

        Ok(LocalModel {
            catalog_id: catalog_match.map(|model| model.id.clone()),
            file_name,
            path: path.display().to_string(),
            size_bytes: stat.len(),
            modified_ms,
            metadata,
            parse_error,
        })
    }
}

/// Ask one file the questions it can answer for itself, and report each answer
/// as pass, fail or skipped.
///
/// This is deliberately header-and-size only. GGUF stores no per-file checksum,
/// so a flipped byte inside tensor data cannot be seen here; what this does catch
/// is a file that is gone, empty, not a model, truncated, or a different size
/// than the catalog published. A file that fails any of those should be
/// re-downloaded rather than repaired.
fn verify_path(path: &Path, catalog: &ModelCatalog) -> ModelVerification {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    let mut checks = Vec::new();

    let size = match std::fs::metadata(path) {
        Ok(stat) if stat.len() == 0 => {
            checks.push(VerificationCheck::failed(
                "File on disk",
                "the file is there but holds no bytes at all",
            ));
            None
        }
        Ok(stat) => {
            checks.push(VerificationCheck::passed(
                "File on disk",
                format!("{} bytes, readable", stat.len()),
            ));
            Some(stat.len())
        }
        Err(error) => {
            checks.push(VerificationCheck::failed(
                "File on disk",
                format!("it cannot be read: {error}"),
            ));
            None
        }
    };

    let header = match GgufHeader::read(path) {
        Ok(header) => {
            checks.push(VerificationCheck::passed(
                "GGUF header",
                format!(
                    "version {}, {} tensor(s), {} metadata key(s)",
                    header.version,
                    header.n_tensors,
                    header.metadata.len()
                ),
            ));
            Some(header)
        }
        Err(error) => {
            checks.push(VerificationCheck::failed("GGUF header", error.to_string()));
            None
        }
    };

    if let Some(header) = header.as_ref() {
        checks.push(verify_tensors(header));
        checks.push(verify_bytes_per_parameter(header));
    } else {
        checks.push(VerificationCheck::skipped(
            "Tensor data",
            "the header did not parse, so nothing inside the file can be located",
        ));
        checks.push(VerificationCheck::skipped(
            "Bytes per parameter",
            "the header did not parse",
        ));
    }

    checks.push(verify_against_catalog(catalog, &file_name, size));

    ModelVerification::new(file_name, path.display().to_string(), checks)
}

/// Are the weight bytes the header says exist actually present?
fn verify_tensors(header: &GgufHeader) -> VerificationCheck {
    let label = "Tensor data";
    if header.n_tensors == 0 {
        return VerificationCheck::skipped(
            label,
            "the file declares no tensors, so there is no data section to place",
        );
    }
    let Some(start) = header.data_start else {
        return VerificationCheck::skipped(
            label,
            "the tensor list could not be read, so the data section cannot be located",
        );
    };
    let data_bytes = header.source_bytes.saturating_sub(start);
    if data_bytes == 0 {
        return VerificationCheck::failed(
            label,
            format!(
                "weights should begin at byte {start} but the file ends at {} — the file is truncated",
                header.source_bytes
            ),
        );
    }
    match header.min_file_bytes() {
        Some(required) if header.source_bytes < required => VerificationCheck::failed(
            label,
            format!(
                "the deepest tensor starts at byte {required} but the file is only {} bytes",
                header.source_bytes
            ),
        ),
        Some(required) => VerificationCheck::passed(
            label,
            format!("{data_bytes} weight bytes from byte {start}, including the tensor that begins at byte {required}"),
        ),
        None => VerificationCheck::passed(label, format!("{data_bytes} bytes of weights")),
    }
}

/// Does the weight section hold a plausible number of bytes per parameter?
fn verify_bytes_per_parameter(header: &GgufHeader) -> VerificationCheck {
    let label = "Bytes per parameter";
    let params = header.total_params.filter(|count| *count > 0);
    match (header.weight_bytes(), params) {
        (Some(weights), Some(count)) => {
            let per_param = weights as f64 / count as f64;
            let measured = format!("{per_param:.2} bytes for each of {count} parameters");
            if (MIN_BYTES_PER_PARAM..=MAX_BYTES_PER_PARAM).contains(&per_param) {
                VerificationCheck::passed(label, measured)
            } else {
                VerificationCheck::failed(
                    label,
                    format!(
                        "{measured} — outside the {:.1}–{:.1} range any quantisation reaches",
                        MIN_BYTES_PER_PARAM, MAX_BYTES_PER_PARAM
                    ),
                )
            }
        }
        (Some(_), None) => VerificationCheck::skipped(
            label,
            "the header does not count its parameters, so plausibility cannot be judged",
        ),
        (None, _) => VerificationCheck::skipped(label, "the weight section could not be measured"),
    }
}

/// Does a catalog file still weigh what Hugging Face publishes for it?
fn verify_against_catalog(
    catalog: &ModelCatalog,
    file_name: &str,
    size: Option<u64>,
) -> VerificationCheck {
    let label = "Catalog size";
    let Some(model) = catalog.find_by_filename(file_name) else {
        return VerificationCheck::skipped(
            label,
            "this file is not in the catalog, so there is no published size to compare",
        );
    };
    let Some(actual) = size else {
        return VerificationCheck::skipped(label, "the file could not be sized");
    };
    if model.size_mb == 0 {
        return VerificationCheck::skipped(
            label,
            format!("the catalog entry for {} records no size", model.id),
        );
    }
    let expected = model.size_mb.saturating_mul(1_000_000);
    if size_matches(actual, model.size_mb) {
        return VerificationCheck::passed(
            label,
            format!(
                "{actual} bytes against the catalog's {expected} for {}",
                model.id
            ),
        );
    }
    VerificationCheck::failed(
        label,
        format!(
            "{actual} bytes on disk, {expected} published for {} — download it again",
            model.id
        ),
    )
}

/// A real file whose bytes start with a GGUF header, as a canonical path.
///
/// Canonicalising is what makes the "is this already in the library" comparison
/// below mean something: a dragged path and a scanned path can name one file.
fn checked_gguf(source: &Path) -> AppResult<PathBuf> {
    let path = source.canonicalize().map_err(|_| {
        AppError::ModelNotFound(format!("nothing to import at {}", source.display()))
    })?;
    if !path.is_file() {
        return Err(AppError::InvalidRequest(format!(
            "{} is a folder, not a model file",
            path.display()
        )));
    }
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default();
    if !extension.eq_ignore_ascii_case("gguf") {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("that file");
        return Err(AppError::InvalidRequest(format!(
            "{name} is not a .gguf file"
        )));
    }
    // The cheapest proof there are model bytes behind the extension: a few
    // kilobytes of header, not the whole file.
    GgufHeader::read(&path)?;
    Ok(path)
}

/// The name to file the copy under: the source's own, unless that belongs to a
/// different file. Overwriting is not on the table — the library holds models
/// the user paid download time for, and two quants of one model share a stem
/// all the time.
fn numerate(base: &str, models_dir: &Path, source: &Path) -> AppResult<String> {
    let existing = models_dir.join(base);
    if !existing.exists() {
        return Ok(base.to_string());
    }
    if std::fs::canonicalize(&existing).is_ok_and(|found| found == source) {
        // The same file, named by the folder it already lives in.
        return Ok(base.to_string());
    }

    // `file_stem` rather than a suffix trim, so an upper-case `.GGUF` is stripped
    // too and the copy does not end up named `Model.GGUF-2.gguf`.
    let stem: String = Path::new(base)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(base)
        .chars()
        .take(180)
        .collect();
    for attempt in 2..=99u8 {
        let candidate = format!("{stem}-{attempt}.gguf");
        if !models_dir.join(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Err(AppError::InvalidRequest(format!(
        "the model folder already holds too many files named {base}"
    )))
}

/// Copy, then prove the copy is the file the library should open.
fn write_copy(source: &Path, temp: &Path, size: u64) -> AppResult<()> {
    std::fs::copy(source, temp)?;
    let written = file_size(temp)?;
    if written != size {
        return Err(AppError::Io(std::io::Error::other(format!(
            "the copy stopped short: {written} of {size} bytes"
        ))));
    }
    // Read back from the copy rather than trusting the source: this is the file
    // that will be loaded, and a full disk mid-copy is exactly how a truncated
    // model gets into a library.
    GgufHeader::read(temp)?;
    Ok(())
}

fn file_size(path: &Path) -> AppResult<u64> {
    Ok(std::fs::metadata(path)?.len())
}

/// GGUF wins for anything it declares; the catalog fills the gaps.
fn merge(mut metadata: ModelMetadata, catalog: Option<&ModelDescriptor>) -> ModelMetadata {
    if let Some(model) = catalog {
        if metadata.name.is_none() {
            metadata.name = Some(model.display_name.clone());
        }
        if metadata.quantization.is_none() {
            metadata.quantization = Some(model.quantization.clone());
        }
        if metadata.license.is_none() {
            metadata.license = Some(model.license.clone());
        }
        if metadata.context_length.is_none() {
            metadata.context_length = Some(model.context_length);
        }
        if metadata.parameters_b.is_none() {
            metadata.parameters_b = Some(model.parameters_b);
        }
    }
    metadata
}

fn metadata_from_catalog(catalog: Option<&ModelDescriptor>, file_name: &str) -> ModelMetadata {
    match catalog {
        Some(model) => ModelMetadata {
            name: Some(model.display_name.clone()),
            architecture: Some(model.family.label().to_string()),
            quantization: Some(model.quantization.clone()),
            parameters_b: Some(model.parameters_b),
            context_length: Some(model.context_length),
            license: Some(model.license.clone()),
            train_type: Some("instruct".to_string()),
            ..Default::default()
        },
        None => ModelMetadata {
            name: Some(file_name.trim_end_matches(".gguf").to_string()),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use smollm_core::model::CheckStatus;

    /// Minimal v3 header: one `general.name` string and no tensors.
    fn gguf_bytes(name: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"GGUF");
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&1u64.to_le_bytes());
        let key = "general.name";
        out.extend_from_slice(&(key.len() as u64).to_le_bytes());
        out.extend_from_slice(key.as_bytes());
        out.extend_from_slice(&8u32.to_le_bytes()); // STRING
        out.extend_from_slice(&(name.len() as u64).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smollm-library-{}-{label}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn scans_files_and_skips_part_files() {
        let models_dir = temp_dir("scan");
        std::fs::write(models_dir.join("alpha.gguf"), gguf_bytes("Alpha Model")).expect("write");
        std::fs::write(models_dir.join("beta.gguf.part"), b"partial").expect("write");
        std::fs::write(models_dir.join("notes.txt"), b"ignore me").expect("write");
        std::fs::create_dir_all(models_dir.join("sub")).expect("dir");
        std::fs::write(
            models_dir.join("sub").join("gamma.gguf"),
            gguf_bytes("Gamma Model"),
        )
        .expect("write");

        let library = ModelLibrary::new(AppPaths::new(Some(models_dir)));
        let catalog = ModelCatalog::embedded();
        let models = library.scan(&catalog).expect("scans");

        let names: Vec<&str> = models
            .iter()
            .map(|model| model.file_name.as_str())
            .collect();
        assert_eq!(names, vec!["alpha.gguf", "gamma.gguf"]);
        assert_eq!(models.len(), 2, "beta.gguf.part must be skipped");
        assert_eq!(models[0].metadata.name.as_deref(), Some("Alpha Model"));
        assert_eq!(models[1].metadata.name.as_deref(), Some("Gamma Model"));
        assert!(models.iter().all(|model| model.parse_error.is_none()));
    }

    #[test]
    fn unreadable_files_report_a_parse_error_but_stay_listed() {
        let models_dir = temp_dir("broken");
        std::fs::write(models_dir.join("broken.gguf"), b"not a gguf file").expect("write");

        let library = ModelLibrary::new(AppPaths::new(Some(models_dir.clone())));
        let catalog = ModelCatalog::embedded();
        let models = library.scan(&catalog).expect("scans");
        assert_eq!(models.len(), 1);
        assert!(models[0].parse_error.is_some());
        assert_eq!(models[0].size_bytes, 15);

        // Catalog filenames are matched case-insensitively for the id link.
        let known = catalog.models().first().expect("catalog is not empty");
        std::fs::write(models_dir.join(&known.filename), b"still not a gguf file").expect("write");
        let models = library.scan(&catalog).expect("rescans");
        let matched = models
            .iter()
            .find(|model| model.catalog_id.is_some())
            .expect("catalog id linked from filename");
        assert_eq!(matched.catalog_id.as_deref(), Some(known.id.as_str()));
        assert!(matched.metadata.name.is_some());
    }

    #[test]
    fn resolve_prefers_catalog_then_filename() {
        let models_dir = temp_dir("resolve");
        let catalog = ModelCatalog::embedded();
        let known = catalog
            .find("qwen2.5-0.5b-instruct-gguf")
            .expect("curated id");
        let library = ModelLibrary::new(AppPaths::new(Some(models_dir.clone())));

        // Missing file: catalog id resolves to "not downloaded".
        let error = library
            .resolve(&catalog, "qwen2.5-0.5b-instruct-gguf")
            .expect_err("needs the file");
        assert!(matches!(error, AppError::ModelNotDownloaded(_)));
        assert!(!library.exists(known));

        std::fs::write(models_dir.join(&known.filename), gguf_bytes("Qwen")).expect("write");
        assert_eq!(
            library
                .resolve(&catalog, "qwen2.5-0.5b-instruct-gguf")
                .expect("now local"),
            models_dir.join(&known.filename)
        );

        // Bare filenames work for private models too.
        std::fs::write(models_dir.join("private-model.gguf"), gguf_bytes("Private"))
            .expect("write");
        assert!(library
            .resolve(&catalog, "private-model.gguf")
            .expect("private file")
            .ends_with("private-model.gguf"));

        assert!(matches!(
            library.resolve(&catalog, "does-not-exist.gguf"),
            Err(AppError::ModelNotFound(_))
        ));
        assert!(library.resolve(&catalog, "../escape.gguf").is_err());
    }

    #[test]
    fn delete_only_touches_the_model_directory() {
        let models_dir = temp_dir("delete");
        let outside = temp_dir("delete-outside");
        let outside_file = outside.join("keep.gguf");
        std::fs::write(&outside_file, gguf_bytes("Keep")).expect("write");
        std::fs::write(models_dir.join("remove.gguf"), gguf_bytes("Remove")).expect("write");

        let library = ModelLibrary::new(AppPaths::new(Some(models_dir.clone())));
        let removed = library.delete("remove.gguf").expect("deleted");
        assert!(!removed.exists());

        // Traversal attempts and unknown names are rejected.
        assert!(library.delete("../keep.gguf").is_err());
        assert!(library.delete("missing.gguf").is_err());
        assert!(outside_file.exists(), "files outside the model dir survive");
    }

    #[test]
    fn imports_a_file_the_user_already_has() {
        let source_dir = temp_dir("import-source");
        let models_dir = temp_dir("import-dest");
        let source = source_dir.join("private-1.7b.gguf");
        std::fs::write(&source, gguf_bytes("Private 1.7B")).expect("write");

        let library = ModelLibrary::new(AppPaths::new(Some(models_dir.clone())));
        let catalog = ModelCatalog::embedded();
        let imported = library.import(&source, &catalog).expect("imports");

        assert_eq!(imported.file_name, "private-1.7b.gguf");
        assert_eq!(imported.metadata.name.as_deref(), Some("Private 1.7B"));
        assert_eq!(imported.size_bytes, gguf_bytes("Private 1.7B").len() as u64);
        assert!(source.exists(), "an import copies the file, never moves it");
        assert!(
            !models_dir.join("private-1.7b.gguf.part").exists(),
            "the staging name is renamed away"
        );

        let names: Vec<String> = library
            .scan(&catalog)
            .expect("scans")
            .iter()
            .map(|model| model.file_name.clone())
            .collect();
        assert_eq!(names, vec!["private-1.7b.gguf".to_string()]);
    }

    #[test]
    fn refuses_anything_that_is_not_a_model() {
        let source_dir = temp_dir("refuse");
        let models_dir = temp_dir("refuse-dest");
        let library = ModelLibrary::new(AppPaths::new(Some(models_dir.clone())));
        let catalog = ModelCatalog::embedded();

        // A header that parses is still not importable under the wrong name.
        let wrong_extension = source_dir.join("notes.txt");
        std::fs::write(&wrong_extension, gguf_bytes("Notes")).expect("write");
        let error = library
            .import(&wrong_extension, &catalog)
            .expect_err("extension is checked");
        assert!(error.to_string().contains("not a .gguf file"));

        let wrong_bytes = source_dir.join("not-a-model.gguf");
        std::fs::write(&wrong_bytes, b"GGUH this is not a header").expect("write");
        let error = library
            .import(&wrong_bytes, &catalog)
            .expect_err("header is checked");
        assert!(error.to_string().contains("bad magic"));

        assert!(library
            .import(&source_dir.join("gone.gguf"), &catalog)
            .is_err());
        assert!(library.import(&source_dir, &catalog).is_err());

        let models = library.scan(&catalog).expect("folder stays clean");
        assert!(models.is_empty(), "a refusal writes nothing");
    }

    #[test]
    fn a_name_clash_keeps_both_files() {
        let source_dir = temp_dir("clash-source");
        let models_dir = temp_dir("clash-dest");
        std::fs::write(
            models_dir.join("same.gguf"),
            gguf_bytes("Already Installed"),
        )
        .expect("write");
        let incoming = source_dir.join("same.gguf");
        std::fs::write(&incoming, gguf_bytes("Fresh From Downloads")).expect("write");

        let library = ModelLibrary::new(AppPaths::new(Some(models_dir.clone())));
        let catalog = ModelCatalog::embedded();
        let imported = library
            .import(&incoming, &catalog)
            .expect("imports as a second copy");

        assert_eq!(imported.file_name, "same-2.gguf");
        assert_eq!(
            imported.metadata.name.as_deref(),
            Some("Fresh From Downloads")
        );
        let held = std::fs::read(models_dir.join("same.gguf")).expect("still there");
        assert_eq!(
            held,
            gguf_bytes("Already Installed"),
            "nothing is overwritten"
        );
    }

    #[test]
    fn a_file_already_in_the_library_is_listed_not_duplicated() {
        let models_dir = temp_dir("already");
        let inside = models_dir.join("resident.gguf");
        std::fs::write(&inside, gguf_bytes("Resident")).expect("write");

        let library = ModelLibrary::new(AppPaths::new(Some(models_dir.clone())));
        let catalog = ModelCatalog::embedded();
        let listed = library.import(&inside, &catalog).expect("lists it");

        assert_eq!(listed.file_name, "resident.gguf");
        assert!(!models_dir.join("resident-2.gguf").exists());
        assert_eq!(library.scan(&catalog).expect("scans").len(), 1);
    }

    #[test]
    fn totals_and_free_space_are_reported() {
        let models_dir = temp_dir("totals");
        std::fs::write(models_dir.join("one.gguf"), gguf_bytes("One")).expect("write");
        let library = ModelLibrary::new(AppPaths::new(Some(models_dir)));
        let catalog = ModelCatalog::embedded();
        let models = library.scan(&catalog).expect("scans");
        assert_eq!(
            library.total_size_bytes(&models),
            gguf_bytes("One").len() as u64
        );
        assert!(library.free_space_bytes().expect("measured") > 0);
    }

    /// v3 header with one tensor of `dims` starting at byte `offset` inside the
    /// data section, then `weight_bytes` of weight data after the aligned start.
    fn model_bytes(dims: &[u32], offset: u64, weight_bytes: usize) -> Vec<u8> {
        let mut out = gguf_bytes("Verify Fixture");
        // Rewrite the counts: one tensor, and the one metadata key already there.
        out[8..16].copy_from_slice(&1u64.to_le_bytes());
        let tensor = "blk.0.weight";
        out.extend_from_slice(&(tensor.len() as u64).to_le_bytes());
        out.extend_from_slice(tensor.as_bytes());
        out.extend_from_slice(&(dims.len() as u32).to_le_bytes());
        for dim in dims {
            out.extend_from_slice(&dim.to_le_bytes());
        }
        out.extend_from_slice(&1u32.to_le_bytes()); // dtype
        out.extend_from_slice(&offset.to_le_bytes());
        while out.len() as u64 % 32 != 0 {
            out.push(0);
        }
        out.resize(out.len() + weight_bytes, 7);
        out
    }

    fn verification_labels(report: &ModelVerification) -> Vec<&str> {
        report
            .checks
            .iter()
            .map(|check| check.label.as_str())
            .collect()
    }

    #[test]
    fn a_whole_model_passes_what_can_be_judged() {
        let models_dir = temp_dir("verify-ok");
        std::fs::write(
            models_dir.join("whole.gguf"),
            model_bytes(&[1_000], 0, 1_000),
        )
        .expect("write");

        let library = ModelLibrary::new(AppPaths::new(Some(models_dir)));
        let catalog = ModelCatalog::embedded();
        let report = library.verify("whole.gguf", &catalog).expect("verifies");

        assert_eq!(report.file_name, "whole.gguf");
        assert!(report.ok, "{:?}", report.checks);
        assert_eq!(report.failures(), 0);
        assert_eq!(
            verification_labels(&report),
            vec![
                "File on disk",
                "GGUF header",
                "Tensor data",
                "Bytes per parameter",
                "Catalog size"
            ]
        );
        // A private file has no published size to be wrong about, which is
        // reported as skipped rather than as a pass.
        assert_eq!(report.checks[4].status, CheckStatus::Skipped);
    }

    #[test]
    fn a_truncated_download_fails_the_tensor_check() {
        let models_dir = temp_dir("verify-truncated");
        // The header claims a tensor begins 5_000 bytes into the data section,
        // but only 1_000 weight bytes were ever written.
        std::fs::write(
            models_dir.join("short.gguf"),
            model_bytes(&[1_000], 5_000, 1_000),
        )
        .expect("write");

        let library = ModelLibrary::new(AppPaths::new(Some(models_dir)));
        let report = library
            .verify("short.gguf", &ModelCatalog::embedded())
            .expect("verifies");

        assert!(!report.ok);
        assert_eq!(report.failures(), 1);
        let failed = report
            .checks
            .iter()
            .find(|check| check.is_failed())
            .expect("one failing check");
        assert_eq!(failed.label, "Tensor data");
        assert!(failed.detail.contains("the file is only"));
    }

    #[test]
    fn a_file_without_any_weight_bytes_is_called_out() {
        let models_dir = temp_dir("verify-header-only");
        std::fs::write(models_dir.join("stub.gguf"), model_bytes(&[1_000], 0, 0)).expect("write");

        let library = ModelLibrary::new(AppPaths::new(Some(models_dir)));
        let report = library
            .verify("stub.gguf", &ModelCatalog::embedded())
            .expect("verifies");

        assert!(!report.ok);
        let tensor = report
            .checks
            .iter()
            .find(|check| check.label == "Tensor data")
            .expect("the check is always reported");
        assert!(tensor.detail.contains("truncated"));
        // Nothing can be said about bytes per parameter without weights.
        assert_eq!(report.checks[3].status, CheckStatus::Skipped);
    }

    #[test]
    fn a_file_that_is_not_gguf_says_no_more_than_is_known() {
        let models_dir = temp_dir("verify-junk");
        std::fs::write(models_dir.join("junk.gguf"), b"GGUZ not a model file").expect("write");

        let library = ModelLibrary::new(AppPaths::new(Some(models_dir)));
        let report = library
            .verify("junk.gguf", &ModelCatalog::embedded())
            .expect("verifies");

        assert!(!report.ok);
        assert_eq!(report.checks[1].status, CheckStatus::Failed);
        assert_eq!(report.checks[2].status, CheckStatus::Skipped);
        assert_eq!(report.checks[3].status, CheckStatus::Skipped);
    }

    #[test]
    fn a_catalog_file_that_changed_size_fails_against_the_catalog() {
        let catalog = ModelCatalog::embedded();
        let known = catalog
            .models()
            .iter()
            .find(|model| model.size_mb > 0)
            .expect("a catalog entry with a size");

        let models_dir = temp_dir("verify-catalog");
        // Whole by its own numbers, a tenth the size Hugging Face publishes.
        std::fs::write(
            models_dir.join(&known.filename),
            model_bytes(&[1_000], 0, 1_000),
        )
        .expect("write");

        let library = ModelLibrary::new(AppPaths::new(Some(models_dir)));
        let report = library.verify(&known.filename, &catalog).expect("verifies");

        assert!(!report.ok);
        let size = report
            .checks
            .iter()
            .find(|check| check.label == "Catalog size")
            .expect("the check is always reported");
        assert_eq!(size.status, CheckStatus::Failed);
        assert!(size.detail.contains("download it again"));
    }

    #[test]
    fn verify_only_answers_for_files_in_the_library() {
        let models_dir = temp_dir("verify-scope");
        std::fs::write(
            models_dir.join("inside.gguf"),
            model_bytes(&[1_000], 0, 1_000),
        )
        .expect("write");
        let library = ModelLibrary::new(AppPaths::new(Some(models_dir)));
        let catalog = ModelCatalog::embedded();

        assert!(library.verify("missing.gguf", &catalog).is_err());
        assert!(library.verify("../inside.gguf", &catalog).is_err());
    }

    #[test]
    fn verify_all_reports_every_scanned_file() {
        let models_dir = temp_dir("verify-all");
        std::fs::write(
            models_dir.join("whole.gguf"),
            model_bytes(&[1_000], 0, 1_000),
        )
        .expect("write");
        std::fs::write(models_dir.join("broken.gguf"), b"not a model").expect("write");
        std::fs::write(models_dir.join("half.gguf.part"), b"ignore me").expect("write");

        let library = ModelLibrary::new(AppPaths::new(Some(models_dir)));
        let catalog = ModelCatalog::embedded();
        let reports = library.verify_all(&catalog).expect("verifies");

        assert_eq!(reports.len(), 2, "a .part file is not a model to verify");
        assert!(reports.iter().any(|report| report.ok));
        let broken = reports
            .iter()
            .find(|report| report.file_name == "broken.gguf")
            .expect("the broken file is reported, not dropped");
        assert!(!broken.ok);
    }
}
