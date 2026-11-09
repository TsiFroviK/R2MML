use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::Duration,
};

pub const CATALOG_URL: &str = "https://thunderstore.io/c/riskofrain2/api/v1/package/";
const CATALOG_LIMIT: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub full_name: String,
    pub owner: String,
    #[serde(default)]
    pub package_url: String,
    #[serde(default)]
    pub date_created: String,
    #[serde(default)]
    pub date_updated: String,
    #[serde(default)]
    pub rating_score: i64,
    #[serde(default)]
    pub is_pinned: bool,
    #[serde(default)]
    pub is_deprecated: bool,
    #[serde(default)]
    pub has_nsfw_content: bool,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub versions: Vec<PackageVersion>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PackageVersion {
    pub name: String,
    pub full_name: String,
    pub version_number: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    pub download_url: String,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub date_created: String,
    #[serde(default)]
    pub website_url: String,
    #[serde(default = "yes")]
    pub is_active: bool,
    #[serde(default)]
    pub file_size: u64,
}
fn yes() -> bool {
    true
}

impl Package {
    pub fn latest(&self) -> Option<&PackageVersion> {
        self.versions.iter().filter(|v| v.is_active).max_by(|a, b| {
            semver::Version::parse(&a.version_number)
                .ok()
                .cmp(&semver::Version::parse(&b.version_number).ok())
        })
    }
    pub fn total_downloads(&self) -> u64 {
        self.versions
            .iter()
            .fold(0u64, |n, v| n.saturating_add(v.downloads))
    }
    pub fn version(&self, version: &str) -> Option<&PackageVersion> {
        self.versions
            .iter()
            .find(|v| v.version_number == version && v.is_active)
    }
}

pub fn client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent(concat!("R2MML/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 8 || !is_thunderstore_url(attempt.url()) {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()?)
}

fn is_thunderstore_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url
            .host_str()
            .is_some_and(|h| h == "thunderstore.io" || h.ends_with(".thunderstore.io"))
}

pub fn fetch_bytes(url: &str, limit: u64) -> Result<Vec<u8>> {
    let url = reqwest::Url::parse(url)?;
    if !is_thunderstore_url(&url) {
        bail!("Unsupported Thunderstore URL: {url}");
    }
    let response = client()?.get(url).send()?.error_for_status()?;
    if response.status().is_redirection() {
        bail!("Download redirected outside Thunderstore");
    }
    if response.content_length().is_some_and(|size| size > limit) {
        bail!("Download exceeds size limit");
    }
    let mut bytes = Vec::new();
    response.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("Download exceeds size limit");
    }
    Ok(bytes)
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("File has no parent directory")?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|e| e.error)?;
    Ok(())
}

fn parse_catalog(bytes: &[u8]) -> Result<Vec<Package>> {
    let packages: Vec<Package> = serde_json::from_slice(bytes)?;
    if packages.is_empty() || packages.len() > 100_000 {
        bail!("Unexpected Thunderstore catalog size");
    }
    Ok(packages)
}

pub fn fetch_catalog(cache_root: &Path) -> Result<Vec<Package>> {
    fetch_catalog_inner(cache_root, false)
}

pub fn fetch_catalog_force(cache_root: &Path) -> Result<Vec<Package>> {
    fetch_catalog_inner(cache_root, true)
}

fn fetch_catalog_inner(cache_root: &Path, force: bool) -> Result<Vec<Package>> {
    fs::create_dir_all(cache_root)?;
    let cache = cache_root.join("riskofrain2-catalog.json");
    if !force
        && fs::metadata(&cache)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < Duration::from_secs(900))
    {
        if let Ok(bytes) = fs::read(&cache) {
            if let Ok(packages) = parse_catalog(&bytes) {
                return Ok(packages);
            }
        }
    }
    match fetch_bytes(CATALOG_URL, CATALOG_LIMIT).and_then(|bytes| {
        let parsed = parse_catalog(&bytes)?;
        atomic_write(&cache, &bytes)?;
        Ok(parsed)
    }) {
        Ok(packages) => Ok(packages),
        Err(error) => {
            if let Ok(bytes) = fs::read(&cache) {
                if let Ok(packages) = parse_catalog(&bytes) {
                    return Ok(packages);
                }
            }
            Err(error).context("Could not load the Thunderstore catalog")
        }
    }
}

pub fn validate_component(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 180
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-+.".contains(&c))
        || value == "."
        || value == ".."
    {
        bail!("Invalid package identifier: {value}");
    }
    Ok(())
}

pub fn fetch_readme(package: &Package, version: &str, cache: &Path) -> Result<String> {
    fetch_document(package, version, cache, "readme")
}

pub fn fetch_changelog(package: &Package, version: &str, cache: &Path) -> Result<String> {
    fetch_document(package, version, cache, "changelog")
}

fn fetch_document(package: &Package, version: &str, cache: &Path, kind: &str) -> Result<String> {
    validate_component(&package.owner)?;
    validate_component(&package.name)?;
    validate_component(version)?;
    let destination = cache
        .join(format!("{kind}s"))
        .join(format!("{}-{version}.md", package.full_name));
    validate_component(&package.full_name)?;
    if let Ok(text) = fs::read_to_string(&destination) {
        return Ok(text);
    }
    let url = format!(
        "https://thunderstore.io/api/experimental/package/{}/{}/{version}/{kind}/",
        package.owner, package.name
    );
    #[derive(Deserialize)]
    struct Readme {
        markdown: Option<String>,
    }
    let readme: Readme = serde_json::from_slice(&fetch_bytes(&url, 4 * 1024 * 1024)?)?;
    let text = readme.markdown.unwrap_or_default();
    atomic_write(&destination, text.as_bytes())?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn url_boundaries() {
        for good in [
            "https://thunderstore.io/package/a",
            "https://ccdn.thunderstore.io/a",
        ] {
            assert!(is_thunderstore_url(&reqwest::Url::parse(good).unwrap()));
        }
        for bad in [
            "http://thunderstore.io/a",
            "https://thunderstore.io.evil.test/a",
            "https://localhost/a",
            "https://x@thunderstore.io/a",
        ] {
            assert!(!is_thunderstore_url(&reqwest::Url::parse(bad).unwrap()));
        }
    }
    #[test]
    fn rejects_path_identifiers() {
        for bad in ["../x", "..", "x/y", "C:foo", "x\\y"] {
            assert!(validate_component(bad).is_err());
        }
        validate_component("RiskofThunder-R2API_Core-5.3.0").unwrap();
    }
}
