//! Isolated Unity assembly registration for a modded game launch.

use anyhow::{Context, Result, ensure};
use serde_json::Value;
#[cfg(target_os = "macos")]
use sha2::{Digest, Sha256};
use std::collections::HashSet;
#[cfg(target_os = "macos")]
use std::ffi::OsString;
#[cfg(target_os = "macos")]
use std::fs::{self, File, OpenOptions};
#[cfg(target_os = "macos")]
use std::io::{self, Write};
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
#[cfg(target_os = "macos")]
use tempfile::{NamedTempFile, TempDir};

#[cfg(target_os = "macos")]
const MANIFEST: &str = "ScriptingAssemblies.json";
#[cfg(target_os = "macos")]
const PLAYER: &str = "Risk of Rain 2 Player";
const USER_ASSEMBLY: i32 = 16;

/// Keep this value alive until the game exits, including failed startup.
#[cfg(target_os = "macos")]
pub(crate) struct StagedGame {
    pub(crate) executable: PathBuf,
    pub(crate) assembly_map: PathBuf,
    pub(crate) assembly_count: usize,
    _directory: TempDir,
}

#[cfg(target_os = "macos")]
impl StagedGame {
    pub(crate) fn install_loader(&self, core: &Path) -> Result<()> {
        const STOCK: &str = "780f175821781c14fdc349bf214603b7bab594ac556aecb1f6679b80248133f0";
        let loader = core.join("BepInEx.dll");
        ensure!(
            format!("{:x}", Sha256::digest(fs::read(&loader)?)) == STOCK,
            "The Unity assembly adapter requires the verified BepInEx 5.4.23.6 runtime"
        );
        let helper = core.join("R2MML.UnityCompatibility.dll");
        ensure!(
            !helper.is_symlink(),
            "Unity adapter destination must be a regular file"
        );
        crate::catalog::atomic_write(
            &helper,
            include_bytes!("../compatibility/unity/R2MML.UnityCompatibility.dll"),
        )?;
        crate::catalog::atomic_write(
            &loader,
            include_bytes!("../compatibility/unity/BepInEx.UnityCompatibility.dll"),
        )?;
        Ok(())
    }
}

/// Redirect two Starstorm hooks to the equivalent overload in Risk2Mac 1.5.0.
/// Both inputs are pinned because the string heap offsets are version-specific.
#[cfg(target_os = "macos")]
pub(super) fn adapt_starstorm_hooks(game: &Path, active: &Path) -> Result<bool> {
    const MOD: &str = "d0d6a547a6a8c9141e6b9dc9371e8de3d5418c14ede2bd5d9dc316a2652f1486";
    const GAME: &str = "5574c0c14c32139c363bf543205917140e0ff3cde3a5edf8f21d748e86fc025c";
    const ADAPTED: &str = "923774bd850bef5431d00b840dffbb585e6c21ad0d998d85a08c4297b94d9251";
    const ORIGINAL_MVID: &[u8] = &[
        0x89, 0xd5, 0x9d, 0xdc, 0x59, 0xad, 0x27, 0x4e, 0x96, 0xe6, 0x89, 0x8e, 0x32, 0xb9, 0x2b,
        0xf8,
    ];
    const ADAPTED_MVID: &[u8] = &[
        0x00, 0xc7, 0xc7, 0x9b, 0x3c, 0x54, 0xca, 0x4f, 0x97, 0x69, 0x09, 0x7e, 0x54, 0x7e, 0x6f,
        0x12,
    ];
    const EDITS: &[(usize, &str, &str)] = &[
        (0xf46c8, "add_Spawn_SpawnArgs", "add_Spawn"),
        (
            0xe99cd,
            "add_Spawn_SpawnCard_EliteDef_Transform_MonsterSpawnDistance_bool_float_PlacementMode_bool",
            "add_Spawn",
        ),
        (
            0xe9a27,
            "orig_Spawn_SpawnCard_EliteDef_Transform_MonsterSpawnDistance_bool_float_PlacementMode_bool",
            "orig_Spawn",
        ),
        (
            0xe9a82,
            "hook_Spawn_SpawnCard_EliteDef_Transform_MonsterSpawnDistance_bool_float_PlacementMode_bool",
            "hook_Spawn",
        ),
    ];
    let file = active.join("plugins/TeamMoonstorm-Starstorm2/Starstorm2.dll");
    if !file
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_file())
    {
        return Ok(false);
    }
    let mut bytes = fs::read(&file)?;
    if format!("{:x}", Sha256::digest(&bytes)) != MOD
        || format!(
            "{:x}",
            Sha256::digest(fs::read(
                game.join("Contents/Resources/Data/Managed/RoR2.dll"),
            )?)
        ) != GAME
    {
        return Ok(false);
    }
    for &(offset, original, replacement) in EDITS {
        let slot = bytes
            .get_mut(offset..=offset + original.len())
            .context("Missing Starstorm hook metadata")?;
        ensure!(
            slot.last() == Some(&0) && slot.starts_with(original.as_bytes()),
            "Unexpected Starstorm hook metadata"
        );
        slot.fill(0);
        slot[..replacement.len()].copy_from_slice(replacement.as_bytes());
    }
    let mvid = bytes
        .get_mut(0x118ecc..0x118ecc + 16)
        .context("Missing Starstorm module identity")?;
    ensure!(
        mvid == ORIGINAL_MVID,
        "Unexpected Starstorm module identity"
    );
    mvid.copy_from_slice(ADAPTED_MVID);
    ensure!(
        format!("{:x}", Sha256::digest(&bytes)) == ADAPTED,
        "Starstorm hook adaptation checksum mismatch"
    );
    crate::catalog::atomic_write(&file, &bytes)?;
    Ok(true)
}

#[cfg(target_os = "macos")]
pub(crate) fn prepare(
    game_app: &Path,
    profile: &Path,
    assemblies: &[PathBuf],
) -> Result<StagedGame> {
    ensure!(
        !assemblies.is_empty(),
        "No assemblies to register with Unity"
    );
    let game_app = game_app
        .canonicalize()
        .context("Resolve the Risk2Mac app")?;
    let profile = profile.canonicalize().context("Resolve the mod profile")?;
    ensure!(
        game_app.extension().is_some_and(|ext| ext == "app") && game_app.is_dir(),
        "Expected a Risk2Mac application bundle"
    );
    ensure!(
        !profile.starts_with(&game_app),
        "The Unity staging directory must be outside the original game"
    );
    let app_name = game_app.file_name().context("Missing game bundle name")?;
    let candidates = assemblies
        .iter()
        .map(|path| {
            ensure!(
                fs::symlink_metadata(path)
                    .with_context(|| format!("Inspect mod assembly {}", path.display()))?
                    .is_file(),
                "Mod assembly must be a regular file: {}",
                path.display()
            );
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .context("Unity assembly filename must be valid UTF-8")?;
            Ok((path, name))
        })
        .collect::<Result<Vec<_>>>()?;
    let sessions = profile.join("Unity Sessions");
    fs::create_dir_all(&sessions)?;
    let sessions = real_directory(&profile, &sessions)?;
    // The launcher holds the profile lock and has checked for a running player.
    // Recover sessions abandoned if the manager was closed before the game.
    for entry in fs::read_dir(&sessions)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && entry.file_name().to_string_lossy().starts_with("session-")
            && entry
                .path()
                .join(".r2mml-unity-session")
                .symlink_metadata()
                .is_ok_and(|kind| kind.is_file())
        {
            fs::remove_dir_all(entry.path())?;
        }
    }
    let directory = tempfile::Builder::new()
        .prefix("session-")
        .tempdir_in(&sessions)
        .context("Create a temporary copy for Unity mod assets")?;
    File::create_new(directory.path().join(".r2mml-unity-session"))?;
    let target = directory.path().join(app_name);
    // macOS cp clones same-volume files when possible and otherwise copies them.
    let copy = Command::new("/bin/cp")
        .arg("-cRP")
        .arg(&game_app)
        .arg(&target)
        .output()
        .context("Copy the Risk2Mac application for modded launch")?;
    ensure!(
        copy.status.success(),
        "Could not stage Risk2Mac ({}): {}",
        copy.status,
        String::from_utf8_lossy(&copy.stderr).trim()
    );

    let staged = real_directory(directory.path(), &target)?;
    let contents = real_directory(&staged, &staged.join("Contents"))?;
    let resources = real_directory(&contents, &contents.join("Resources"))?;
    let data = real_directory(&resources, &resources.join("Data"))?;
    let managed = real_directory(&data, &data.join("Managed"))?;
    let macos = real_directory(&contents, &contents.join("MacOS"))?;
    let executable = macos.join(PLAYER);
    let registry_path = data.join(MANIFEST);
    for path in [&executable, &registry_path] {
        ensure!(
            fs::symlink_metadata(path)
                .with_context(|| format!("Inspect {}", path.display()))?
                .is_file(),
            "Expected a regular game file: {}",
            path.display()
        );
    }

    let registry = fs::read(&registry_path).context("Read the staged Unity assembly manifest")?;
    let mut occupied = HashSet::new();
    for entry in fs::read_dir(&managed).context("List the staged Unity Managed directory")? {
        let filename: OsString = entry?.file_name();
        if let Some(filename) = filename.to_str() {
            occupied.insert(filename.to_lowercase());
        }
    }
    // Some packages include framework assemblies for older installations.
    // Unity keeps its own copies and BepInEx still sees the original mod tree.
    let candidates = candidates
        .into_iter()
        .filter(|(_, name)| !occupied.contains(&name.to_lowercase()))
        .collect::<Vec<_>>();
    let names = candidates.iter().map(|(_, name)| *name).collect::<Vec<_>>();
    let assembly_count = names.len();
    let registry = updated_manifest(&registry, &names, occupied)?;
    let assembly_map = directory.path().join("UnityAssemblies.txt");
    let mut map_file = File::create_new(&assembly_map)?;
    for (source, name) in candidates {
        let destination = managed.join(name);
        writeln!(
            map_file,
            "{}\t{}",
            map_path(source)?,
            map_path(&destination)?
        )?;
        let mut input = File::open(source)
            .with_context(|| format!("Open mod assembly {}", source.display()))?;
        // create_new refuses existing files, case-folded aliases and symlinks.
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .with_context(|| format!("Create staged Unity assembly {name}"))?;
        io::copy(&mut input, &mut output)
            .with_context(|| format!("Stage Unity assembly {name}"))?;
    }
    let mut temporary =
        NamedTempFile::new_in(&data).context("Create the Unity manifest replacement")?;
    serde_json::to_writer(temporary.as_file_mut(), &registry)
        .context("Write the Unity assembly manifest")?;
    temporary
        .persist(&registry_path)
        .context("Replace the Unity assembly manifest in the staged app")?;
    Ok(StagedGame {
        executable,
        assembly_map,
        assembly_count,
        _directory: directory,
    })
}

#[cfg(target_os = "macos")]
fn map_path(path: &Path) -> Result<&str> {
    let name = path
        .to_str()
        .context("Unity assembly path must be valid UTF-8")?;
    ensure!(
        path.is_absolute() && !name.contains(['\t', '\n', '\r']),
        "Invalid Unity assembly path: {name}"
    );
    Ok(name)
}

#[cfg(target_os = "macos")]
fn real_directory(parent: &Path, path: &Path) -> Result<PathBuf> {
    ensure!(
        fs::symlink_metadata(path)
            .with_context(|| format!("Inspect staged directory {}", path.display()))?
            .is_dir(),
        "Expected a real staged directory: {}",
        path.display()
    );
    let resolved = path.canonicalize()?;
    ensure!(
        resolved.parent() == Some(parent),
        "Staged directory escaped its parent: {}",
        path.display()
    );
    Ok(resolved)
}

fn updated_manifest(input: &[u8], added: &[&str], mut occupied: HashSet<String>) -> Result<Value> {
    let mut value: Value =
        serde_json::from_slice(input).context("Parse Unity assembly manifest")?;
    let manifest = value
        .as_object_mut()
        .context("Expected a Unity manifest object")?;
    let names = manifest
        .get("names")
        .and_then(Value::as_array)
        .context("Unity manifest has no names array")?;
    let types = manifest
        .get("types")
        .and_then(Value::as_array)
        .context("Unity manifest has no types array")?;
    ensure!(
        names.len() == types.len(),
        "Unity manifest arrays have different lengths"
    );
    let mut originals = HashSet::new();
    for name in names {
        let name = name
            .as_str()
            .context("Unity manifest has a non-string name")?;
        let key = assembly_filename_key(name)?;
        ensure!(
            originals.insert(key),
            "Unity manifest lists {name} more than once"
        );
    }
    for kind in types {
        ensure!(
            kind.as_i64()
                .is_some_and(|kind| i32::try_from(kind).is_ok()),
            "Unity manifest has an invalid assembly type"
        );
    }
    occupied.extend(originals);
    for name in added {
        let key = assembly_filename_key(name)?;
        ensure!(
            occupied.insert(key),
            "Unity assembly filename conflicts: {name}"
        );
    }

    if let Some(Value::Array(names)) = manifest.get_mut("names") {
        names.extend(added.iter().map(|name| Value::from(*name)));
    }
    if let Some(Value::Array(types)) = manifest.get_mut("types") {
        types.extend(added.iter().map(|_| Value::from(USER_ASSEMBLY)));
    }
    Ok(value)
}

fn assembly_filename_key(name: &str) -> Result<String> {
    let key = name.to_lowercase();
    ensure!(
        key.len() > 4
            && key.ends_with(".dll")
            && !name
                .chars()
                .any(|c| matches!(c, '/' | '\\' | ':') || c.is_control()),
        "Expected a Unity assembly filename: {name}"
    );
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn appends_to_valid_manifest_and_rejects_malformed_or_conflicting_names() {
        let manifest = br#"{"names":["RoR2.dll"],"types":[2],"extra":{"kept":true}}"#;
        let updated = updated_manifest(manifest, &["Starstorm2.dll"], HashSet::new()).unwrap();
        assert_eq!(
            updated,
            json!({"names":["RoR2.dll","Starstorm2.dll"],"types":[2,16],"extra":{"kept":true}})
        );
        for malformed in [
            br#"{"names":["RoR2.dll"],"types":[]}"#.as_slice(),
            br#"{"names":["RoR2.dll"],"types":["2"]}"#,
            br#"{"names":["RoR2.dll","ror2.dll"],"types":[2,2]}"#,
        ] {
            assert!(updated_manifest(malformed, &[], HashSet::new()).is_err());
        }
        for added in [
            &["ror2.dll"][..],
            &["first.dll", "FIRST.DLL"],
            &["../mod.dll"],
        ] {
            assert!(updated_manifest(manifest, added, HashSet::new()).is_err());
        }
        assert!(
            updated_manifest(
                manifest,
                &["MOD.dll"],
                HashSet::from(["mod.dll".to_string()])
            )
            .is_err()
        );
    }
}
