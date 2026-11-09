//! Launch-time BepInEx injection. The selected game bundle is read-only.
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const RUNTIME_VERSION: &str = "5.4.23.6";
pub const RUNTIME_URL: &str = "https://github.com/BepInEx/BepInEx/releases/download/v5.4.23.6/BepInEx_macos_universal_5.4.23.6.zip";
const RUNTIME_SHA256: &str = "2e711e21f6570da964fce4654b1976500dbe043f61778bce0008500078564638";
const MAX_RUNTIME_ARCHIVE: u64 = 4 * 1024 * 1024;
const PLAYER_NAME: &str = "Risk of Rain 2 Player";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameInfo {
    pub app_path: PathBuf,
    pub executable: PathBuf,
    pub architecture: String,
    pub steam: bool,
}

pub fn validate_game(game: &Path) -> Result<GameInfo> {
    let app = game
        .canonicalize()
        .context("Choose an existing Risk2Mac .app")?;
    ensure!(
        app.extension().is_some_and(|e| e == "app") && app.is_dir(),
        "Select Risk of Rain 2.app, not its executable"
    );
    let contents = app.join("Contents");
    let managed = contents.join("Resources/Data/Managed");
    for name in [
        "RoR2.dll",
        "RoR2Arm64.ManagedSolo.dll",
        "RoR2Arm64.OfflinePlatform.dll",
    ] {
        ensure!(
            managed.join(name).is_file(),
            "This is not a supported Risk2Mac port: missing {name}"
        );
    }
    let executable = contents.join("MacOS").join(PLAYER_NAME);
    let arches =
        macho_architectures(&fs::read(&executable).context("Missing native Risk2Mac player")?)?;
    let preferred = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x86_64"
    };
    let architecture = if arches.iter().any(|a| a == preferred) {
        preferred.to_owned()
    } else {
        arches[0].clone()
    };
    #[cfg(target_os = "macos")]
    {
        // An Intel build of the manager may itself run under Rosetta, so
        // inspect the hardware rather than equating process and host CPU.
        let host_has_arm64 = cfg!(target_arch = "aarch64")
            || Command::new("/usr/sbin/sysctl")
                .args(["-n", "hw.optional.arm64"])
                .output()
                .is_ok_and(|output| output.status.success() && output.stdout == b"1\n");
        require_compatible_host(&architecture, host_has_arm64)?;
    }
    let info = plist::Value::from_file(contents.join("Info.plist"))
        .context("Invalid application information")?;
    let launcher = info
        .as_dictionary()
        .and_then(|d| d.get("CFBundleExecutable"))
        .and_then(|v| v.as_string())
        .context("Application has no launcher")?;
    ensure!(
        Path::new(launcher).components().count() == 1
            && contents.join("MacOS").join(launcher).is_file(),
        "Application launcher is missing"
    );
    Ok(GameInfo {
        app_path: app,
        executable,
        architecture,
        steam: contents
            .join("Plugins/libRisk2MacSteamBridge.dylib")
            .is_file(),
    })
}

#[cfg(any(target_os = "macos", test))]
fn require_compatible_host(game_architecture: &str, host_has_arm64: bool) -> Result<()> {
    ensure!(
        game_architecture != "arm64" || host_has_arm64,
        "This game build requires Apple Silicon. Choose an x86_64 Risk2Mac build for this Intel Mac."
    );
    Ok(())
}

fn macho_architectures(data: &[u8]) -> Result<Vec<String>> {
    ensure!(data.len() >= 8, "Native executable is truncated");
    let be = |at: usize| -> Result<u32> {
        Ok(u32::from_be_bytes(
            data.get(at..at + 4)
                .context("Truncated Mach-O header")?
                .try_into()?,
        ))
    };
    let le = |at: usize| -> Result<u32> {
        Ok(u32::from_le_bytes(
            data.get(at..at + 4)
                .context("Truncated Mach-O header")?
                .try_into()?,
        ))
    };
    let mut cpus = Vec::new();
    match be(0)? {
        0xcafebabe | 0xcafebabf => {
            let stride = if be(0)? == 0xcafebabf { 32 } else { 20 };
            let count = be(4)? as usize;
            ensure!(count <= 16, "Unexpected Mach-O architecture count");
            for index in 0..count {
                cpus.push(be(8 + index * stride)?);
            }
        }
        0xcffaedfe => cpus.push(le(4)?),
        0xfeedfacf => cpus.push(be(4)?),
        _ => bail!("The game player is not a macOS 64-bit executable"),
    }
    let mut result = Vec::new();
    for cpu in cpus {
        let arch = match cpu {
            0x01000007 => "x86_64",
            0x0100000c => "arm64",
            _ => continue,
        };
        if !result.iter().any(|a| a == arch) {
            result.push(arch.to_owned());
        }
    }
    ensure!(
        !result.is_empty(),
        "Only Intel x86_64 and Apple Silicon games are supported"
    );
    Ok(result)
}

/// Download the official, pinned macOS runtime into the manager's cache.
pub fn ensure_runtime(cache_root: &Path, progress: &dyn Fn(&str)) -> Result<PathBuf> {
    fs::create_dir_all(cache_root)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache_root.join("runtime.lock"))?;
    lock.try_lock_exclusive()
        .context("The mod runtime is already being prepared by another process")?;
    let archive_path = cache_root.join(format!("BepInEx-macos-{RUNTIME_VERSION}.zip"));
    let archive = if archive_path.is_file() {
        ensure!(
            archive_path.metadata()?.len() <= MAX_RUNTIME_ARCHIVE,
            "Cached runtime archive is larger than expected"
        );
        fs::read(&archive_path)?
    } else {
        progress("Downloading BepInEx for macOS…");
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(180))
            .user_agent("R2MML/0.1.0")
            .build()?;
        let response = client.get(RUNTIME_URL).send()?.error_for_status()?;
        ensure!(
            response.content_length().unwrap_or(0) <= MAX_RUNTIME_ARCHIVE,
            "Runtime download is larger than expected"
        );
        let mut bytes = Vec::new();
        response
            .take(MAX_RUNTIME_ARCHIVE + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_RUNTIME_ARCHIVE,
            "Runtime download is too large"
        );
        verify_archive(&bytes)?;
        let mut temporary = tempfile::NamedTempFile::new_in(cache_root)?;
        temporary.write_all(&bytes)?;
        temporary.persist(&archive_path).map_err(|e| e.error)?;
        bytes
    };
    verify_archive(&archive)?;
    let runtime = cache_root.join(format!("BepInEx-macos-{RUNTIME_VERSION}"));
    let mut zip = zip::ZipArchive::new(Cursor::new(&archive))?;
    if runtime.exists() {
        verify_extracted(&mut zip, &runtime)?;
        return Ok(runtime);
    }
    progress("Preparing the macOS mod runtime…");
    let temporary = tempfile::tempdir_in(cache_root)?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        if entry.is_dir() || !runtime_member(entry.name()) {
            continue;
        }
        let relative = entry
            .enclosed_name()
            .context("Unsafe runtime archive path")?;
        ensure!(
            !entry.is_symlink() && entry.size() < 16 * 1024 * 1024,
            "Unexpected runtime archive member"
        );
        let output = temporary.path().join(relative);
        fs::create_dir_all(output.parent().context("Invalid runtime path")?)?;
        let mut file = File::create(&output)?;
        std::io::copy(&mut entry, &mut file)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                &output,
                fs::Permissions::from_mode(if output.extension().is_some_and(|e| e == "dylib") {
                    0o755
                } else {
                    0o644
                }),
            )?;
        }
    }
    verify_extracted(&mut zip, temporary.path())?;
    fs::rename(temporary.path(), &runtime)?;
    Ok(runtime)
}

fn verify_archive(bytes: &[u8]) -> Result<()> {
    ensure!(
        format!("{:x}", Sha256::digest(bytes)) == RUNTIME_SHA256,
        "BepInEx download failed its SHA-256 integrity check"
    );
    Ok(())
}

fn runtime_member(name: &str) -> bool {
    name == "libdoorstop.dylib" || name.starts_with("BepInEx/core/")
}

fn verify_extracted(zip: &mut zip::ZipArchive<Cursor<&Vec<u8>>>, directory: &Path) -> Result<()> {
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        if entry.is_dir() || !runtime_member(entry.name()) {
            continue;
        }
        let relative = entry
            .enclosed_name()
            .context("Unsafe runtime archive path")?;
        let target = directory.join(relative);
        ensure!(
            target.is_file() && !target.is_symlink(),
            "Runtime file missing or replaced: {}",
            target.display()
        );
        let mut expected = Vec::new();
        entry.read_to_end(&mut expected)?;
        ensure!(
            Sha256::digest(fs::read(&target)?) == Sha256::digest(&expected),
            "Runtime cache is damaged: {}",
            target.display()
        );
    }
    ensure!(
        directory.join("libdoorstop.dylib").is_file()
            && directory
                .join("BepInEx/core/BepInEx.Preloader.dll")
                .is_file(),
        "Incomplete macOS runtime"
    );
    Ok(())
}

fn assert_external_profile(profile: &Path, game: &Path) -> Result<PathBuf> {
    let mut ancestor = profile;
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .context("Profile has no existing parent directory")?;
    }
    ensure!(
        !ancestor.canonicalize()?.starts_with(game),
        "The mod profile must be outside the game application"
    );
    ensure!(
        !profile
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir)),
        "The profile path must not contain parent-directory components"
    );
    fs::create_dir_all(profile)?;
    let resolved = profile.canonicalize()?;
    ensure!(
        !resolved.starts_with(game),
        "The mod profile must be outside the game application"
    );
    for name in ["BepInEx", "BepInEx/core", "logs"] {
        let target = resolved.join(name);
        ensure!(
            !target.is_symlink(),
            "Profile folders must not be symbolic links: {}",
            target.display()
        );
    }
    Ok(resolved)
}

fn copy_runtime_core(runtime: &Path, profile: &Path) -> Result<()> {
    let source = runtime.join("BepInEx/core");
    let target = profile.join("BepInEx/core");
    fs::create_dir_all(&target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        ensure!(
            entry.file_type()?.is_file(),
            "Unexpected runtime core entry"
        );
        let destination = target.join(entry.file_name());
        ensure!(
            !destination.is_symlink(),
            "Runtime core destination is a symbolic link"
        );
        fs::copy(entry.path(), destination)?;
    }
    Ok(())
}

fn clean_loader_environment(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        let text = key.to_string_lossy();
        if text.starts_with("DOORSTOP_")
            || text.starts_with("MONOMOD_")
            || text.starts_with("BEPINEX_")
            || matches!(
                text.as_ref(),
                "DYLD_INSERT_LIBRARIES" | "DYLD_LIBRARY_PATH" | "STEAM_DYLD_INSERT_LIBRARIES"
            )
        {
            command.env_remove(key);
        }
    }
}

pub fn launch_game(game: &Path, profile: &Path, runtime: &Path, modded: bool) -> Result<u32> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (game, profile, runtime, modded);
        bail!("Launching the Risk2Mac port requires macOS");
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::process::CommandExt;
        let info = validate_game(game)?;
        let processes = Command::new("/bin/ps")
            .args(["-axo", "pid=,comm="])
            .output()?;
        ensure!(
            !String::from_utf8_lossy(&processes.stdout)
                .lines()
                .any(|line| line.trim_end().ends_with(&format!("/{PLAYER_NAME}"))),
            "Close Risk of Rain 2 before starting another session"
        );
        let contents = info.app_path.join("Contents");
        if !modded {
            let plist = plist::Value::from_file(contents.join("Info.plist"))?;
            let launcher = plist
                .as_dictionary()
                .and_then(|d| d.get("CFBundleExecutable"))
                .and_then(|v| v.as_string())
                .context("Missing application launcher")?;
            let mut command = Command::new(contents.join("MacOS").join(launcher));
            clean_loader_environment(&mut command);
            command
                .current_dir(&contents)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0);
            return Ok(command
                .spawn()
                .context("Could not start the vanilla game")?
                .id());
        }
        let profile = assert_external_profile(profile, &info.app_path)?;
        ensure!(
            !runtime.canonicalize()?.starts_with(&info.app_path),
            "The mod runtime must be outside the game application"
        );
        ensure!(
            runtime.join("libdoorstop.dylib").is_file(),
            "Prepare the macOS runtime before launching"
        );
        crate::install::materialize(&profile)?;
        copy_runtime_core(runtime, &profile)?;
        let logs = profile.join("logs");
        fs::create_dir_all(&logs)?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let unity_log = logs.join(format!("Unity-{stamp}.log"));
        let mut launcher_log = File::create(logs.join(format!("Launcher-{stamp}.log")))?;
        writeln!(
            launcher_log,
            "R2MML modded launch\nGame: {}\nProfile: {}\nArchitecture: {}\nBepInEx: {}",
            info.app_path.display(),
            profile.display(),
            info.architecture,
            RUNTIME_VERSION
        )?;
        let mut libraries = vec![runtime.join("libdoorstop.dylib")];
        let mut command = Command::new(&info.executable);
        clean_loader_environment(&mut command);
        command.env("UNITY_DISPLAYLINK_BLIT", "0");
        if info.steam {
            configure_steam_overlay(&info, &mut command, &mut libraries, &mut launcher_log)?;
        }
        let joined = libraries
            .iter()
            .map(|p| p.to_string_lossy())
            .collect::<Vec<_>>()
            .join(":");
        ensure!(
            !runtime.to_string_lossy().contains(':'),
            "The runtime path cannot contain a colon"
        );
        command
            .env("DYLD_INSERT_LIBRARIES", joined)
            .env("DYLD_LIBRARY_PATH", runtime)
            .env("DOORSTOP_ENABLED", "1")
            .env(
                "DOORSTOP_TARGET_ASSEMBLY",
                profile.join("BepInEx/core/BepInEx.Preloader.dll"),
            )
            .env("DOORSTOP_IGNORE_DISABLED_ENV", "1")
            .args(["--arm64-solo-probe", "--disableCrossplay", "-logFile"])
            .arg(&unity_log)
            .current_dir(&profile)
            .stdin(Stdio::null())
            .stdout(Stdio::from(launcher_log.try_clone()?))
            .stderr(Stdio::from(launcher_log))
            .process_group(0);
        Ok(command
            .spawn()
            .context("Could not start the modded game")?
            .id())
    }
}

#[cfg(target_os = "macos")]
fn configure_steam_overlay(
    info: &GameInfo,
    command: &mut Command,
    libraries: &mut Vec<PathBuf>,
    log: &mut File,
) -> Result<()> {
    if std::env::var("RISK2MAC_FORCE_LOCAL").as_deref() == Ok("1") {
        return Ok(());
    }
    let uid = Command::new("/usr/bin/id").arg("-u").output()?;
    let uid = String::from_utf8_lossy(&uid.stdout);
    if !Command::new("/usr/bin/pgrep")
        .args(["-u", uid.trim(), "-x", "steam_osx"])
        .stdout(Stdio::null())
        .status()?
        .success()
    {
        return Ok(());
    }
    let marker = info
        .app_path
        .join("Contents/Resources/Risk2MacSteamAppId.txt");
    let app_id = if marker.exists() {
        ensure!(
            !marker.is_symlink() && marker.metadata()?.len() <= 7,
            "Invalid Steam session marker"
        );
        let value = fs::read_to_string(&marker)?;
        match value.trim() {
            "632360" => "632360",
            "480" => "480",
            _ => bail!("Unknown Steam session marker"),
        }
    } else {
        "632360"
    };
    let home = PathBuf::from(std::env::var_os("HOME").context("No home directory")?);
    let renderer = home.join("Library/Application Support/Steam/Steam.AppBundle/Steam/Contents/MacOS/gameoverlayrenderer.dylib");
    if renderer.is_file()
        && Command::new("/usr/bin/codesign")
            .args([
                "--verify",
                "--strict",
                "--architecture",
                &info.architecture,
                "-R",
                "=anchor apple generic and certificate leaf[subject.OU] = \"MXGJJ98X76\"",
            ])
            .arg(&renderer)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success()
    {
        libraries.push(renderer);
        command
            .env("SteamAppId", app_id)
            .env("SteamGameId", app_id)
            .env("SteamOverlayGameId", app_id);
        writeln!(log, "Steam overlay enabled for AppID {app_id}")?;
    } else {
        writeln!(
            log,
            "Steam overlay unavailable; game can continue without it"
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recognizes_both_native_architectures() {
        let mut bytes = vec![0u8; 32];
        bytes[..4].copy_from_slice(&0xfeedfacfu32.to_le_bytes());
        bytes[4..8].copy_from_slice(&0x0100000cu32.to_le_bytes());
        assert_eq!(macho_architectures(&bytes).unwrap(), ["arm64"]);
        bytes[4..8].copy_from_slice(&0x01000007u32.to_le_bytes());
        assert_eq!(macho_architectures(&bytes).unwrap(), ["x86_64"]);
    }
    #[test]
    fn rejects_non_macho_and_unknown_cpu() {
        assert!(macho_architectures(b"MZnot-a-mac-executable").is_err());
        let mut bytes = vec![0u8; 32];
        bytes[..4].copy_from_slice(&0xfeedfacfu32.to_le_bytes());
        assert!(macho_architectures(&bytes).is_err());
    }
    #[test]
    fn rejects_runtime_hash_mismatch() {
        assert!(verify_archive(b"not the pinned release").is_err());
    }
    #[test]
    fn arm_game_requires_arm_hardware_not_arm_manager() {
        assert!(require_compatible_host("arm64", false).is_err());
        assert!(require_compatible_host("arm64", true).is_ok());
        assert!(require_compatible_host("x86_64", true).is_ok());
        assert!(require_compatible_host("x86_64", false).is_ok());
    }
    #[test]
    fn profile_cannot_live_in_game_bundle() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("Game.app");
        fs::create_dir(&game).unwrap();
        assert!(
            assert_external_profile(&game.join("mods"), &game.canonicalize().unwrap()).is_err()
        );
        assert!(!game.join("mods").exists());
        assert!(
            assert_external_profile(&root.path().join("profile"), &game.canonicalize().unwrap())
                .is_ok()
        );
    }
}
