//! Local model library: scan, inspect and delete downloaded GGUF files.

use std::path::{Path, PathBuf};

use smollm_core::error::{AppError, AppResult};
use smollm_core::gguf::GgufHeader;
use smollm_core::model::{LocalModel, ModelDescriptor, ModelMetadata};
use smollm_core::paths::AppPaths;

use crate::catalog::ModelCatalog;
use crate::download::{free_space, gb, partial_path_for, sanitize_file_name};

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

    pub fn exists(&self, model: &ModelDescriptor) -> bool {
        self.paths.models_dir.join(&model.filename).exists()
    }

    pub fn local_path(&self, model: &ModelDescriptor) -> PathBuf {
        self.paths.models_dir.join(&model.filename)
    }

    /// Delete a local file. Only files inside the model directory can be removed.
    pub fn delete(&self, file_name: &str) -> AppResult<PathBuf> {
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
                "refusing to delete outside the model directory: {}",
                canonical.display()
            )));
        }
        if canonical.is_dir() {
            return Err(AppError::InvalidRequest(
                "model path is a directory, not a file".into(),
            ));
        }
        std::fs::remove_file(&canonical)?;
        tracing::info!(path = %canonical.display(), "deleted local model");
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
}
