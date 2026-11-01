//! Port of `download_if_necessary` from `pocket_tts/utils/utils.py`: `hf://` paths go through the
//! Hugging Face cache, shared with the Python package, and `http(s)://` URLs through
//! `~/.cache/pocket_tts`.

use anyhow::{Result, bail};
#[cfg(feature = "download")]
use anyhow::Context;
#[cfg(feature = "download")]
use sha2::{Digest, Sha256};
#[cfg(feature = "download")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

/// pal-tts change: a folder holding `hf://` files as `<owner>/<repo>/<file>`, used instead of downloading; a
/// file missing there is downloaded if the `download` feature is on.
static LOCAL: OnceLock<PathBuf> = OnceLock::new();

/// pal-tts change: see [`LOCAL`]; needed without the `download` feature, e.g. on Android.
pub fn use_local_files(dir: PathBuf) {
    let _ = LOCAL.set(dir);
}

pub fn download_if_necessary(file_path: &str) -> Result<PathBuf> {
    if file_path.starts_with("http://") || file_path.starts_with("https://") {
        download_url(file_path)
    } else if let Some(rest) = file_path.strip_prefix("hf://") {
        let (repo_id, filename, revision) = split_hf_path(rest)?;
        if let Some(dir) = LOCAL.get() {
            let path = dir.join(repo_id).join(&filename);
            if path.exists() {
                return Ok(path);
            }
            if !cfg!(feature = "download") {
                bail!("{} is missing", path.display());
            }
        }
        download_hf(repo_id, &filename, revision)
            .map_err(|e| e.context(format!("cannot download {file_path}")))
    } else {
        Ok(PathBuf::from(file_path))
    }
}

/// `owner/name/path/in/repo[@revision]` into its parts.
fn split_hf_path(path: &str) -> Result<(&str, String, Option<&str>)> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 3 {
        bail!("`hf://{path}` should look like hf://<owner>/<repo>/<file>[@revision]")
    }
    let repo_end = parts[0].len() + 1 + parts[1].len();
    let repo_id = &path[..repo_end];
    let filename = &path[repo_end + 1..];
    match filename.split_once('@') {
        Some((filename, revision)) => Ok((repo_id, filename.to_string(), Some(revision))),
        None => Ok((repo_id, filename.to_string(), None)),
    }
}

#[cfg(not(feature = "download"))]
fn download_hf(repo_id: &str, filename: &str, _: Option<&str>) -> Result<PathBuf> {
    bail!("{repo_id}/{filename}: downloads are off (pal-tts `download` feature), see use_local_files")
}

#[cfg(not(feature = "download"))]
fn download_url(url: &str) -> Result<PathBuf> {
    bail!("{url}: downloads are off (pal-tts `download` feature)")
}

#[cfg(feature = "download")]
/// The client reads `HF_TOKEN`, `HF_ENDPOINT`, `HF_HOME` and the token `hf auth login` stores.
fn download_hf(repo_id: &str, filename: &str, revision: Option<&str>) -> Result<PathBuf> {
    let client = hf_hub::HFClientSync::new()?;
    let (owner, name) = hf_hub::split_id(repo_id);
    let path = client
        .model(owner, name)
        .download_file()
        .filename(filename)
        .maybe_revision(revision.map(str::to_string))
        .send()?;
    Ok(path)
}

#[cfg(feature = "download")]
pub fn cache_directory() -> Result<PathBuf> {
    let home = std::env::home_dir().context("cannot locate the home directory")?;
    let dir = home.join(".cache").join("pocket_tts");
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    Ok(dir)
}

#[cfg(feature = "download")]
fn download_url(url: &str) -> Result<PathBuf> {
    let url_path = url.split(['?', '#']).next().unwrap_or(url);
    let url_path = url_path.splitn(4, '/').nth(3).unwrap_or("");
    let suffix = Path::new(url_path)
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default();
    let digest = Sha256::digest(url.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let cached_file = cache_directory()?.join(format!("{hex}{suffix}"));
    if !cached_file.exists() {
        let response = reqwest::blocking::get(url)
            .and_then(|r| r.error_for_status())
            .with_context(|| format!("cannot download {url}"))?;
        let bytes = response
            .bytes()
            .with_context(|| format!("cannot download {url}"))?;
        let partial = cached_file.with_extension("partial");
        std::fs::write(&partial, &bytes)?;
        std::fs::rename(&partial, &cached_file)?;
    }
    Ok(cached_file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hf_paths_split_like_python() {
        assert_eq!(
            split_hf_path("kyutai/pocket-tts/languages/english/model.safetensors@abc").unwrap(),
            (
                "kyutai/pocket-tts",
                "languages/english/model.safetensors".to_string(),
                Some("abc")
            )
        );
        assert_eq!(
            split_hf_path("user/repo/config.yaml").unwrap(),
            ("user/repo", "config.yaml".to_string(), None)
        );
        assert!(split_hf_path("user/repo").is_err());
    }
}
