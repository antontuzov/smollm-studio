//! Model discovery for SmolLLM Studio: curated catalog, Hugging Face
//! resolution, downloads and the local library.

pub mod catalog;
pub mod download;
pub mod hf;
pub mod library;

pub use catalog::{CatalogFilters, ModelCatalog, SortKey};
pub use download::{
    free_space, sanitize_file_name, size_matches, DownloadCancelled, DownloadCompleted,
    DownloadEvent, DownloadFailedEvent, DownloadManager, DownloadProgress, DownloadState,
    DownloadTask,
};
pub use hf::{metadata_url, resolve_url, HfClient, RemoteFile};
pub use library::ModelLibrary;

use smollm_core::paths::AppPaths;

/// One-call convenience used by the desktop app and the CLI.
pub fn load_catalog(paths: &AppPaths) -> ModelCatalog {
    ModelCatalog::load(paths)
}
