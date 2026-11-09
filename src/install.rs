use crate::catalog::{Package, PackageVersion, atomic_write, fetch_bytes, validate_component};
use anyhow::{Context, Result, bail};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::Cursor,
    path::{Path, PathBuf},
};

const PROVIDED_RUNTIME: &str = "bbepis-BepInExPack";
const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
const MAX_EXPANDED: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct InstalledMod {
    pub full_name: String,
    pub name: String,
    pub owner: String,
    pub version: String,
    pub enabled: bool,
    #[serde(default)]
    pub explicit: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProfileState {
    #[serde(default)]
    pub installed: Vec<InstalledMod>,
    /// Dependencies the user explicitly chose to leave disabled while their
    /// dependents remain enabled. Missing files and incompatible versions are
    /// still errors; this only records the user's enabled-state choice.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub disabled_dependency_overrides: BTreeSet<String>,
}

impl ProfileState {
    pub fn load(profile: &Path) -> Result<Self> {
        let file = profile.join("profile.json");
        if !file.exists() {
            return Ok(Self::default());
        }
        let state: Self = serde_json::from_slice(&fs::read(file)?)?;
        let mut names = BTreeSet::new();
        for package in &state.installed {
            validate_component(&package.full_name)?;
            Version::parse(&package.version)?;
            if !names.insert(&package.full_name) {
                bail!("Duplicate installed package: {}", package.full_name);
            }
        }
        for name in &state.disabled_dependency_overrides {
            validate_component(name)?;
            if !state.installed.iter().any(|p| p.full_name == *name && !p.enabled) {
                bail!("Disabled dependency override does not name a disabled installed mod: {name}");
            }
        }
        Ok(state)
    }
    fn save(&self, profile: &Path) -> Result<()> {
        atomic_write(
            &profile.join("profile.json"),
            &serde_json::to_vec_pretty(self)?,
        )
    }
}

#[derive(Clone, Debug)]
pub struct PlannedPackage {
    pub package: Package,
    pub version: PackageVersion,
}
#[derive(Clone, Debug)]
pub struct InstallPlan {
    pub packages: Vec<PlannedPackage>,
}

pub fn dependency(reference: &str) -> Result<(String, Version)> {
    let (name, version) = reference
        .rsplit_once('-')
        .context("Dependency has no version")?;
    validate_component(name)?;
    if !name.contains('-') {
        bail!("Dependency has no owner: {reference}");
    }
    Ok((
        name.to_string(),
        Version::parse(version).with_context(|| format!("Invalid dependency: {reference}"))?,
    ))
}

/// The selected version is pinned. Dependency strings are minimum versions;
/// an already installed newer version is retained instead of downgraded.
pub fn plan_install(
    package: &Package,
    version: &str,
    catalog: &[Package],
    profile: &Path,
) -> Result<InstallPlan> {
    let state = ProfileState::load(profile)?;
    let catalog: BTreeMap<&str, &Package> =
        catalog.iter().map(|p| (p.full_name.as_str(), p)).collect();
    let root_version = package
        .version(version)
        .context("Selected version is unavailable")?;
    let pinned = Version::parse(version)?;
    let existing: BTreeMap<&str, &InstalledMod> = state
        .installed
        .iter()
        .map(|p| (p.full_name.as_str(), p))
        .collect();
    let mut selected: BTreeMap<String, PlannedPackage> = BTreeMap::new();
    selected.insert(
        package.full_name.clone(),
        PlannedPackage {
            package: package.clone(),
            version: root_version.clone(),
        },
    );
    let mut requirements: BTreeMap<String, Version> = BTreeMap::new();
    let mut queue: VecDeque<String> = VecDeque::from([package.full_name.clone()]);
    // Existing enabled mods constrain a version change, so an update cannot
    // silently leave another enabled package with an unsatisfied dependency.
    for installed in state
        .installed
        .iter()
        .filter(|p| p.enabled && p.full_name != package.full_name)
    {
        for dep in &installed.dependencies {
            let (name, minimum) = dependency(dep)?;
            // An unrelated installation must not silently undo a confirmed
            // "disable only" choice. New packages' own requirements below
            // still install and enable the dependencies they need.
            if state.disabled_dependency_overrides.contains(&name) {
                continue;
            }
            requirements
                .entry(name)
                .and_modify(|v| *v = v.clone().max(minimum.clone()))
                .or_insert(minimum);
        }
    }
    let mut iterations = 0;
    loop {
        while let Some(name) = queue.pop_front() {
            iterations += 1;
            if iterations > 10_000 || selected.len() > 1000 {
                bail!("Dependency graph is too large");
            }
            let current = &selected[&name];
            // The native macOS BepInEx runtime is the provider for this Windows
            // distribution. Its bundled fork-specific extras are not implied
            // by that substitution; normal mods' direct requirements still are.
            if name == PROVIDED_RUNTIME {
                continue;
            }
            for dep in &current.version.dependencies {
                let (name, minimum) = dependency(dep)?;
                requirements
                    .entry(name)
                    .and_modify(|v| *v = v.clone().max(minimum.clone()))
                    .or_insert(minimum);
            }
        }
        let mut changed = false;
        for (name, minimum) in &requirements {
            if name == &package.full_name && &pinned < minimum {
                bail!(
                    "{} {} is required by an enabled mod; selected version is {}",
                    name,
                    minimum,
                    pinned
                );
            }
            if selected.get(name).is_some_and(|p| {
                Version::parse(&p.version.version_number).is_ok_and(|v| v >= *minimum)
            }) {
                continue;
            }
            let dependency_package = catalog
                .get(name.as_str())
                .with_context(|| format!("Dependency not found: {name}"))?;
            let existing_version = existing
                .get(name.as_str())
                .and_then(|p| Version::parse(&p.version).ok());
            let reusable = existing_version
                .filter(|v| v >= minimum)
                .and_then(|v| dependency_package.version(&v.to_string()));
            let version = reusable
                .or_else(|| {
                    dependency_package
                        .versions
                        .iter()
                        .filter(|v| v.is_active)
                        .filter_map(|v| {
                            Version::parse(&v.version_number)
                                .ok()
                                .map(|parsed| (parsed, v))
                        })
                        .filter(|(parsed, _)| parsed >= minimum)
                        .max_by(|(a, _), (b, _)| a.cmp(b))
                        .map(|(_, v)| v)
                })
                .with_context(|| format!("No version of {name} satisfies {minimum}"))?;
            selected.insert(
                name.clone(),
                PlannedPackage {
                    package: (*dependency_package).clone(),
                    version: version.clone(),
                },
            );
            queue.push_back(name.clone());
            changed = true;
        }
        if !changed {
            break;
        }
    }
    // Cycles are allowed in Thunderstore's package metadata. The monotonic
    // version constraints above converge without recursive traversal.
    Ok(InstallPlan {
        packages: selected.into_values().collect(),
    })
}

fn archive_for(version: &PackageVersion, cache: &Path) -> Result<Vec<u8>> {
    validate_component(&version.full_name)?;
    let file = cache
        .join("downloads")
        .join(format!("{}.zip", version.full_name));
    let digest_file = file.with_extension("sha256");
    if let (Ok(bytes), Ok(digest)) = (fs::read(&file), fs::read_to_string(&digest_file)) {
        if format!("{:x}", Sha256::digest(&bytes)) == digest.trim() {
            return Ok(bytes);
        }
    }
    let bytes = fetch_bytes(&version.download_url, MAX_ARCHIVE)?;
    if version.file_size > 0 && version.file_size != bytes.len() as u64 {
        bail!(
            "Archive size differs from Thunderstore metadata: {}",
            version.full_name
        );
    }
    atomic_write(&file, &bytes)?;
    atomic_write(
        &digest_file,
        format!("{:x}\n", Sha256::digest(&bytes)).as_bytes(),
    )?;
    Ok(bytes)
}

fn safe_member(name: &str) -> Result<PathBuf> {
    let name = name.replace('\\', "/");
    if name.is_empty() || name.starts_with('/') || name.contains(':') || name.contains('\0') {
        bail!("Unsafe archive path: {name}");
    }
    let mut path = PathBuf::new();
    let components: Vec<_> = name.trim_end_matches('/').split('/').collect();
    if components.len() > 64 || name.len() > 4096 {
        bail!("Archive path is too long");
    }
    for component in components {
        if component.is_empty() || component == "." || component == ".." {
            bail!("Unsafe archive path: {name}");
        }
        if component.len() > 255 {
            bail!("Archive path component is too long");
        }
        path.push(component);
    }
    Ok(path)
}

fn extract_archive(bytes: &[u8], destination: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    if archive.len() > 25_000 {
        bail!("Archive has too many entries");
    }
    let mut expanded = 0u64;
    let mut paths = BTreeSet::new();
    for index in 0..archive.len() {
        let mut member = archive.by_index(index)?;
        let relative = safe_member(member.name())?;
        if let Some(mode) = member.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != 0o100000 && kind != 0o040000 {
                bail!("Archive contains a link or special file: {}", member.name());
            }
        }
        if member.is_dir() {
            continue;
        }
        let normalized = relative.to_string_lossy().to_lowercase();
        if !paths.insert(normalized) {
            bail!("Archive contains duplicate paths: {}", member.name());
        }
        expanded = expanded
            .checked_add(member.size())
            .context("Archive size overflow")?;
        if expanded > MAX_EXPANDED {
            bail!("Expanded archive exceeds 2 GiB");
        }
        let target = destination.join(relative);
        fs::create_dir_all(target.parent().unwrap())?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        std::io::copy(&mut member, &mut output)?;
    }
    Ok(())
}

pub fn install(
    package: &Package,
    version: &str,
    catalog: &[Package],
    profile: &Path,
    cache: &Path,
    progress: impl Fn(&str),
) -> Result<ProfileState> {
    fs::create_dir_all(profile)?;
    let plan = plan_install(package, version, catalog, profile)?;
    let mut state = ProfileState::load(profile)?;
    let packages = profile.join("packages");
    fs::create_dir_all(&packages)?;
    for item in &plan.packages {
        validate_component(&item.package.full_name)?;
        validate_component(&item.version.full_name)?;
        progress(&item.version.full_name);
        if item.package.full_name != PROVIDED_RUNTIME {
            let destination = packages.join(&item.version.full_name);
            if !destination.join(".r2mml-complete").is_file() {
                if destination.exists() {
                    bail!("Incomplete package directory: {}", destination.display());
                }
                let temporary = tempfile::Builder::new()
                    .prefix(".install-")
                    .tempdir_in(&packages)?;
                extract_archive(&archive_for(&item.version, cache)?, temporary.path())?;
                fs::write(
                    temporary.path().join(".r2mml-complete"),
                    &item.version.full_name,
                )?;
                fs::rename(temporary.path(), &destination)?;
            }
        }
        let explicit = item.package.full_name == package.full_name
            || state
                .installed
                .iter()
                .any(|p| p.full_name == item.package.full_name && p.explicit);
        let installed = InstalledMod {
            full_name: item.package.full_name.clone(),
            name: item.package.name.clone(),
            owner: item.package.owner.clone(),
            version: item.version.version_number.clone(),
            enabled: true,
            explicit,
            description: item.version.description.clone(),
            icon: item.version.icon.clone(),
            dependencies: if item.package.full_name == PROVIDED_RUNTIME {
                Vec::new()
            } else {
                item.version.dependencies.clone()
            },
        };
        state
            .installed
            .retain(|p| p.full_name != installed.full_name);
        state.disabled_dependency_overrides.remove(&installed.full_name);
        state.installed.push(installed);
    }
    state
        .installed
        .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    validate_enabled_dependencies(&state)?;
    state.save(profile)?;
    Ok(state)
}

fn validate_enabled_dependencies(state: &ProfileState) -> Result<()> {
    for package in state.installed.iter().filter(|p| p.enabled) {
        for dependency_string in &package.dependencies {
            let (name, required) = dependency(dependency_string)?;
            let installed = state
                .installed
                .iter()
                .find(|p| p.full_name == name)
                .with_context(|| format!("{} requires enabled dependency {name}", package.name))?;
            if !installed.enabled && !state.disabled_dependency_overrides.contains(&name) {
                bail!("{} requires enabled dependency {name}", package.name);
            }
            if Version::parse(&installed.version)? < required {
                bail!("{} requires {name} {required}", package.name);
            }
        }
    }
    Ok(())
}

pub fn toggle(profile: &Path, full_name: &str, enabled: bool) -> Result<ProfileState> {
    let mut state = ProfileState::load(profile)?;
    let package = state
        .installed
        .iter_mut()
        .find(|p| p.full_name == full_name)
        .context("Mod is not installed")?;
    package.enabled = enabled;
    state.disabled_dependency_overrides.remove(full_name);
    validate_enabled_dependencies(&state)?;
    state.save(profile)?;
    Ok(state)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisableMode {
    AllDependents,
    OnlySelected,
}

/// The selected mod followed by its enabled reverse-dependency closure.
/// Disabled dependents do not participate, and each mod appears once even
/// when metadata contains cycles or shared dependencies.
pub fn disable_plan(state: &ProfileState, full_name: &str) -> Result<Vec<String>> {
    if !state.installed.iter().any(|p| p.full_name == full_name) {
        bail!("Mod is not installed");
    }
    let mut reverse: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for package in state.installed.iter().filter(|p| p.enabled) {
        for reference in &package.dependencies {
            let (name, _) = dependency(reference)?;
            reverse.entry(name).or_default().insert(package.full_name.clone());
        }
    }
    let mut affected = BTreeSet::from([full_name.to_owned()]);
    let mut queue = VecDeque::from([full_name.to_owned()]);
    while let Some(name) = queue.pop_front() {
        if let Some(dependents) = reverse.get(&name) {
            for dependent in dependents {
                if affected.insert(dependent.clone()) {
                    queue.push_back(dependent.clone());
                }
            }
        }
    }
    affected.remove(full_name);
    Ok(std::iter::once(full_name.to_owned()).chain(affected).collect())
}

/// Recompute the affected mods from the current profile and commit one update.
pub fn disable(profile: &Path, full_name: &str, mode: DisableMode) -> Result<ProfileState> {
    let mut state = ProfileState::load(profile)?;
    let plan = disable_plan(&state, full_name)?;
    let affected: BTreeSet<&str> = match mode {
        DisableMode::AllDependents => plan.iter().map(String::as_str).collect(),
        DisableMode::OnlySelected => BTreeSet::from([full_name]),
    };
    for package in &mut state.installed {
        if affected.contains(package.full_name.as_str()) {
            package.enabled = false;
            state.disabled_dependency_overrides.remove(&package.full_name);
        }
    }
    if mode == DisableMode::OnlySelected && plan.len() > 1 {
        state.disabled_dependency_overrides.insert(full_name.to_owned());
    }
    validate_enabled_dependencies(&state)?;
    state.save(profile)?;
    Ok(state)
}

pub fn remove(profile: &Path, full_name: &str) -> Result<ProfileState> {
    let mut state = ProfileState::load(profile)?;
    if state.installed.iter().any(|p| {
        p.full_name != full_name
            && p.dependencies
                .iter()
                .any(|dep| dependency(dep).is_ok_and(|(name, _)| name == full_name))
    }) {
        bail!("Other installed mods depend on {full_name}; remove them first");
    }
    state.installed.retain(|p| p.full_name != full_name);
    state.disabled_dependency_overrides.remove(full_name);
    state.save(profile)?;
    Ok(state)
}

fn is_managed_pe(path: &Path) -> Result<bool> {
    let bytes = fs::read(path)?;
    if !bytes.starts_with(b"MZ") {
        return Ok(false);
    }
    let read_u32 = |offset: usize| -> Option<u32> {
        bytes
            .get(offset..offset + 4)
            .map(|v| u32::from_le_bytes(v.try_into().unwrap()))
    };
    let pe = read_u32(0x3c).context("Invalid PE header")? as usize;
    if bytes.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Ok(false);
    }
    let optional = pe + 24;
    let directory = match bytes.get(optional..optional + 2) {
        Some([0x0b, 0x01]) => optional + 96,
        Some([0x0b, 0x02]) => optional + 112,
        _ => return Ok(false),
    };
    Ok(read_u32(directory + 14 * 8).unwrap_or(0) != 0
        && read_u32(directory + 14 * 8 + 4).unwrap_or(0) >= 0x48)
}

fn list_files(root: &Path, directory: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            bail!(
                "Package contains a symbolic link: {}",
                entry.path().display()
            );
        }
        if kind.is_dir() {
            list_files(root, &entry.path(), output)?;
        } else if kind.is_file() {
            output.push(entry.path().strip_prefix(root)?.to_owned());
        } else {
            bail!("Package contains a special file");
        }
    }
    Ok(())
}

fn mapped_path(relative: &Path, package: &str) -> Result<Option<PathBuf>> {
    let components: Vec<String> = relative
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let name = components
        .last()
        .context("Empty package path")?
        .to_lowercase();
    if name == ".r2mml-complete"
        || matches!(
            name.as_str(),
            "manifest.json"
                | "icon.png"
                | "readme.md"
                | "changelog.md"
                | "license"
                | "license.txt"
                | "license.md"
        )
    {
        return Ok(None);
    }
    let start = components
        .iter()
        .position(|p| p.eq_ignore_ascii_case("BepInEx"))
        .map(|i| i + 1)
        .unwrap_or(0);
    let parts = &components[start..];
    if parts.is_empty() {
        return Ok(None);
    }
    let category = parts[0].to_lowercase();
    if category == "core" {
        bail!("{package} attempts to replace the manager's macOS runtime");
    }
    if matches!(
        category.as_str(),
        "plugins" | "patchers" | "monomod" | "config"
    ) {
        let mut path = PathBuf::from(category);
        // Preserve each package's relative layout but isolate plugin/patcher
        // roots to prevent two unrelated packages from overwriting one another.
        if !parts[0].eq_ignore_ascii_case("config") {
            path.push(package);
        }
        for part in &parts[1..] {
            path.push(part);
        }
        return Ok(Some(path));
    }
    if matches!(
        name.as_str(),
        "winhttp.dll"
            | "winmm.dll"
            | "version.dll"
            | "doorstop_config.ini"
            | ".doorstop_version"
            | "run_bepinex.sh"
    ) || name.ends_with(".exe")
        || name.ends_with(".bat")
        || name.ends_with(".cmd")
    {
        return Ok(None);
    }
    let mut path = PathBuf::from("plugins");
    path.push(package);
    for part in parts {
        path.push(part);
    }
    Ok(Some(path))
}

/// Rebuild active plugin trees only immediately before a manager-owned launch.
/// Runtime core, configuration and game files are never replaced here.
pub fn materialize(profile: &Path) -> Result<PathBuf> {
    let state = ProfileState::load(profile)?;
    validate_enabled_dependencies(&state)?;
    fs::create_dir_all(profile)?;
    let bepinex = profile.join("BepInEx");
    fs::create_dir_all(&bepinex)?;
    let temporary = tempfile::Builder::new()
        .prefix(".active-")
        .tempdir_in(profile)?;
    let mut outputs = BTreeSet::new();
    for package in state
        .installed
        .iter()
        .filter(|p| p.enabled && p.full_name != PROVIDED_RUNTIME)
    {
        let source = profile
            .join("packages")
            .join(format!("{}-{}", package.full_name, package.version));
        if !source.join(".r2mml-complete").is_file() {
            bail!("Package files are missing: {}", package.full_name);
        }
        let mut files = Vec::new();
        list_files(&source, &source, &mut files)?;
        for relative in files {
            let Some(mapped) = mapped_path(&relative, &package.full_name)? else {
                continue;
            };
            let from = source.join(&relative);
            if relative
                .extension()
                .is_some_and(|v| v.eq_ignore_ascii_case("dll"))
                && !is_managed_pe(&from)?
            {
                bail!(
                    "{} contains a native Windows DLL ({}) and cannot run on macOS; disable this mod",
                    package.name,
                    relative.display()
                );
            }
            let key = mapped.to_string_lossy().to_lowercase();
            if !outputs.insert(key) {
                bail!("Conflicting mod files: {}", mapped.display());
            }
            let target = temporary.path().join(mapped);
            fs::create_dir_all(target.parent().unwrap())?;
            fs::copy(from, target)?;
        }
    }
    // User configuration survives toggles and updates; packages only seed
    // missing defaults. All validation finishes before touching active trees.
    let configs = temporary.path().join("config");
    if configs.is_dir() {
        let mut files = Vec::new();
        list_files(&configs, &configs, &mut files)?;
        for relative in files {
            let target = bepinex.join("config").join(&relative);
            if !target.exists() {
                fs::create_dir_all(target.parent().unwrap())?;
                fs::copy(configs.join(relative), target)?;
            }
        }
    }
    let mut swapped: Vec<(PathBuf, PathBuf, bool)> = Vec::new();
    let result = (|| -> Result<()> {
        for category in ["plugins", "patchers", "monomod"] {
            let staged = temporary.path().join(category);
            fs::create_dir_all(&staged)?;
            let target = bepinex.join(category);
            let backup = temporary.path().join(format!(".previous-{category}"));
            let existed = target.exists();
            if existed {
                fs::rename(&target, &backup)?;
            }
            swapped.push((target.clone(), backup, existed));
            fs::rename(staged, target)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        for (target, backup, existed) in swapped.into_iter().rev() {
            if target.exists() {
                fs::remove_dir_all(&target)?;
            }
            if existed {
                fs::rename(backup, target)?;
            }
        }
        return Err(error);
    }
    Ok(bepinex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    #[ignore = "Downloads the live Thunderstore catalog and two mod packages"]
    fn live_propersave_install_and_materialize() {
        let dir = tempfile::tempdir().unwrap();
        let cache = std::env::var_os("R2MML_TEST_CACHE")
            .map(PathBuf::from)
            .unwrap_or_else(|| dir.path().join("cache"));
        let catalog = crate::catalog::fetch_catalog(&cache).unwrap();
        let package = catalog
            .iter()
            .find(|p| p.full_name == "KingEnderBrine-ProperSave")
            .unwrap();
        let profile = dir.path().join("profile");
        let installed = install(package, "3.1.3", &catalog, &profile, &cache, |p| {
            println!("Installing {p}")
        })
        .unwrap();
        assert_eq!(installed.installed.len(), 3);
        let runtime = installed
            .installed
            .iter()
            .find(|p| p.full_name == PROVIDED_RUNTIME)
            .unwrap();
        assert!(runtime.dependencies.is_empty());
        let active = materialize(&profile).unwrap();
        let mut files = Vec::new();
        list_files(&active, &active, &mut files).unwrap();
        assert!(
            files
                .iter()
                .any(|p| p.file_name().is_some_and(|n| n == "ProperSave.dll"))
        );
        assert!(files.iter().any(|p| {
            p.file_name()
                .is_some_and(|n| n == "BepInEx.MonoMod.HookGenPatcher.dll")
        }));
        assert!(!active.join("core").exists());
        let config = active.join("config/user-settings.cfg");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, b"preserve").unwrap();
        toggle(&profile, &package.full_name, false).unwrap();
        materialize(&profile).unwrap();
        let mut disabled_files = Vec::new();
        list_files(&active, &active, &mut disabled_files).unwrap();
        assert!(
            !disabled_files
                .iter()
                .any(|p| p.file_name().is_some_and(|n| n == "ProperSave.dll"))
        );
        assert_eq!(fs::read(config).unwrap(), b"preserve");
        println!(
            "Live catalog packages: {}; installed profile: {}",
            catalog.len(),
            serde_json::to_string(&installed).unwrap()
        );
    }
    fn package(name: &str, versions: &[(&str, &[&str])]) -> Package {
        Package {
            full_name: format!("Test-{name}"),
            name: name.into(),
            owner: "Test".into(),
            versions: versions
                .iter()
                .map(|(v, deps)| PackageVersion {
                    name: name.into(),
                    full_name: format!("Test-{name}-{v}"),
                    version_number: (*v).into(),
                    dependencies: deps.iter().map(|d| (*d).into()).collect(),
                    is_active: true,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }
    #[test]
    fn closure_raises_shared_minimum_and_handles_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let a = package("A", &[("1.0.0", &["Test-B-1.0.0", "Test-C-1.0.0"])]);
        let b = package("B", &[("1.0.0", &[]), ("2.0.0", &["Test-A-1.0.0"])]);
        let c = package("C", &[("1.0.0", &["Test-B-2.0.0"])]);
        let plan = plan_install(&a, "1.0.0", &[a.clone(), b, c], dir.path()).unwrap();
        assert_eq!(plan.packages.len(), 3);
        assert_eq!(
            plan.packages
                .iter()
                .find(|p| p.package.full_name == "Test-B")
                .unwrap()
                .version
                .version_number,
            "2.0.0"
        );
    }
    #[test]
    fn pinned_root_conflict_fails() {
        let dir = tempfile::tempdir().unwrap();
        let a = package("A", &[("1.0.0", &["Test-B-1.0.0"])]);
        let b = package("B", &[("1.0.0", &["Test-A-2.0.0"])]);
        assert!(plan_install(&a, "1.0.0", &[a.clone(), b], dir.path()).is_err());
    }
    #[test]
    fn provided_runtime_does_not_pull_windows_distribution_extras() {
        let dir = tempfile::tempdir().unwrap();
        let a = package("A", &[("1.0.0", &["bbepis-BepInExPack-5.4.1905"])]);
        let mut runtime = package(
            "Runtime",
            &[("5.4.2122", &["Windows-DistributionExtra-1.0.0"])],
        );
        runtime.full_name = PROVIDED_RUNTIME.into();
        let plan = plan_install(&a, "1.0.0", &[a.clone(), runtime], dir.path()).unwrap();
        assert_eq!(plan.packages.len(), 2);
    }
    #[test]
    fn extraction_does_not_escape_destination() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("../escaped.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"forbidden").unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let output = dir.path().join("package");
        fs::create_dir(&output).unwrap();
        assert!(extract_archive(&bytes, &output).is_err());
        assert!(!dir.path().join("escaped.txt").exists());
    }
    #[test]
    fn rejects_traversal_on_both_platforms() {
        for path in [
            "../escape",
            "a/../../escape",
            "a\\..\\escape",
            "/absolute",
            "C:\\absolute",
            "a//b",
        ] {
            assert!(safe_member(path).is_err(), "{path}");
        }
        assert_eq!(
            safe_member("plugins/Mod/file.dll").unwrap(),
            PathBuf::from("plugins/Mod/file.dll")
        );
    }
    #[test]
    fn cannot_disable_a_required_mod() {
        let dir = tempfile::tempdir().unwrap();
        let state = ProfileState {
            installed: vec![
                InstalledMod {
                    full_name: "Test-A".into(),
                    version: "1.0.0".into(),
                    enabled: true,
                    dependencies: vec!["Test-B-1.0.0".into()],
                    ..Default::default()
                },
                InstalledMod {
                    full_name: "Test-B".into(),
                    version: "1.0.0".into(),
                    enabled: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        state.save(dir.path()).unwrap();
        let before = fs::read(dir.path().join("profile.json")).unwrap();
        assert!(!String::from_utf8_lossy(&before).contains("disabled_dependency_overrides"));
        assert!(ProfileState::load(dir.path()).unwrap().disabled_dependency_overrides.is_empty());
        assert!(toggle(dir.path(), "Test-B", false).is_err());
        assert!(remove(dir.path(), "Test-B").is_err());
        assert_eq!(before, fs::read(dir.path().join("profile.json")).unwrap());
    }

    fn installed(name: &str, enabled: bool, dependencies: &[&str]) -> InstalledMod {
        InstalledMod {
            full_name: format!("Test-{name}"),
            name: name.into(),
            owner: "Test".into(),
            version: "1.0.0".into(),
            enabled,
            dependencies: dependencies.iter().map(|s| (*s).to_owned()).collect(),
            ..Default::default()
        }
    }

    fn stage_test_package(profile: &Path, name: &str) {
        let directory = profile.join("packages").join(format!("Test-{name}-1.0.0"));
        fs::create_dir_all(directory.join("plugins")).unwrap();
        fs::write(directory.join(".r2mml-complete"), b"complete").unwrap();
        fs::write(directory.join("plugins/payload.txt"), name.as_bytes()).unwrap();
    }

    #[test]
    fn disabling_plan_includes_transitive_dependents_once_and_ignores_disabled_mods() {
        let state = ProfileState {
            installed: vec![
                installed("A", true, &["Test-B-1.0.0"]),
                installed("B", true, &["Test-C-1.0.0"]),
                installed("C", true, &["Test-A-1.0.0"]),
                installed("D", true, &["Test-B-1.0.0", "Test-C-1.0.0"]),
                installed("E", false, &["Test-C-1.0.0"]),
                installed("Unrelated", true, &[]),
            ],
            ..Default::default()
        };
        assert_eq!(disable_plan(&state, "Test-C").unwrap(), ["Test-C", "Test-A", "Test-B", "Test-D"]);
        assert!(disable_plan(&state, "Test-Missing").is_err());
    }

    #[test]
    fn disable_all_changes_only_enabled_reverse_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let state = ProfileState {
            installed: vec![
                installed("A", true, &["Test-B-1.0.0"]),
                installed("B", true, &["Test-C-1.0.0"]),
                installed("C", true, &[]),
                installed("PreviouslyDisabled", false, &["Test-C-1.0.0"]),
                installed("Unrelated", true, &[]),
            ],
            ..Default::default()
        };
        state.save(dir.path()).unwrap();
        let updated = disable(dir.path(), "Test-C", DisableMode::AllDependents).unwrap();
        assert_eq!(updated.installed.iter().filter(|p| p.enabled).map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Unrelated"]);
        assert!(updated.disabled_dependency_overrides.is_empty());
        assert_eq!(serde_json::to_value(updated).unwrap(), serde_json::to_value(ProfileState::load(dir.path()).unwrap()).unwrap());
    }

    #[test]
    fn disable_only_survives_restart_and_materializes_dependents_without_disabled_package() {
        let dir = tempfile::tempdir().unwrap();
        let state = ProfileState {
            installed: vec![installed("A", true, &["Test-B-1.0.0"]), installed("B", true, &[])],
            ..Default::default()
        };
        state.save(dir.path()).unwrap();
        stage_test_package(dir.path(), "A");
        stage_test_package(dir.path(), "B");
        let active = materialize(dir.path()).unwrap();
        assert!(active.join("plugins/Test-B/payload.txt").is_file());
        disable(dir.path(), "Test-B", DisableMode::OnlySelected).unwrap();
        let restarted = ProfileState::load(dir.path()).unwrap();
        assert!(restarted.installed[0].enabled);
        assert!(!restarted.installed[1].enabled);
        assert!(restarted.disabled_dependency_overrides.contains("Test-B"));
        materialize(dir.path()).unwrap();
        assert!(active.join("plugins/Test-A/payload.txt").is_file());
        assert!(!active.join("plugins/Test-B").exists());
        toggle(dir.path(), "Test-A", false).unwrap();
        let reenabled = toggle(dir.path(), "Test-B", true).unwrap();
        assert!(reenabled.disabled_dependency_overrides.is_empty());
        toggle(dir.path(), "Test-A", true).unwrap();
    }

    #[test]
    fn disable_all_clears_prior_only_selected_override() {
        let dir = tempfile::tempdir().unwrap();
        let state = ProfileState {
            installed: vec![installed("A", true, &["Test-B-1.0.0"]), installed("B", true, &[])],
            ..Default::default()
        };
        state.save(dir.path()).unwrap();
        disable(dir.path(), "Test-B", DisableMode::OnlySelected).unwrap();
        let updated = disable(dir.path(), "Test-B", DisableMode::AllDependents).unwrap();
        assert!(updated.installed.iter().all(|p| !p.enabled));
        assert!(updated.disabled_dependency_overrides.is_empty());
    }

    #[test]
    fn disable_only_never_ignores_version_or_missing_dependency_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = ProfileState {
            installed: vec![installed("A", true, &["Test-B-2.0.0"]), installed("B", true, &[]), installed("Unrelated", true, &[])],
            ..Default::default()
        };
        state.save(dir.path()).unwrap();
        let before = fs::read(dir.path().join("profile.json")).unwrap();
        assert!(disable(dir.path(), "Test-B", DisableMode::OnlySelected).is_err());
        assert_eq!(before, fs::read(dir.path().join("profile.json")).unwrap());
        state.installed.retain(|p| p.full_name != "Test-B");
        state.save(dir.path()).unwrap();
        let before = fs::read(dir.path().join("profile.json")).unwrap();
        assert!(disable(dir.path(), "Test-Unrelated", DisableMode::OnlySelected).is_err());
        assert_eq!(before, fs::read(dir.path().join("profile.json")).unwrap());
    }

    #[test]
    fn unrelated_install_does_not_reenable_explicitly_disabled_dependency() {
        let dir = tempfile::tempdir().unwrap();
        let state = ProfileState {
            installed: vec![installed("A", true, &["Test-B-1.0.0"]), installed("B", true, &[])],
            ..Default::default()
        };
        state.save(dir.path()).unwrap();
        disable(dir.path(), "Test-B", DisableMode::OnlySelected).unwrap();
        let c = package("C", &[("1.0.0", &[])]);
        let b = package("B", &[("1.0.0", &[])]);
        let plan = plan_install(&c, "1.0.0", &[c.clone(), b.clone()], dir.path()).unwrap();
        assert_eq!(plan.packages.len(), 1);
        stage_test_package(dir.path(), "C");
        let updated = install(&c, "1.0.0", &[c.clone(), b.clone()], dir.path(), &dir.path().join("cache"), |_| {}).unwrap();
        assert!(updated.disabled_dependency_overrides.contains("Test-B"));
        assert!(!updated.installed.iter().find(|p| p.full_name == "Test-B").unwrap().enabled);
        // Explicitly reinstalling the overridden dependency cannot bypass an
        // enabled mod's minimum version, and failure leaves the profile intact.
        let downgrade = package("B", &[("0.5.0", &[])]);
        let old_files = dir.path().join("packages/Test-B-0.5.0");
        fs::create_dir_all(&old_files).unwrap();
        fs::write(old_files.join(".r2mml-complete"), b"complete").unwrap();
        let before = fs::read(dir.path().join("profile.json")).unwrap();
        assert!(install(&downgrade, "0.5.0", &[downgrade.clone()], dir.path(), &dir.path().join("cache"), |_| {}).is_err());
        assert_eq!(before, fs::read(dir.path().join("profile.json")).unwrap());
        stage_test_package(dir.path(), "B");
        let reinstalled = install(&b, "1.0.0", &[b.clone()], dir.path(), &dir.path().join("cache"), |_| {}).unwrap();
        assert!(reinstalled.disabled_dependency_overrides.is_empty());
        assert!(reinstalled.installed.iter().find(|p| p.full_name == "Test-B").unwrap().enabled);
    }
}
