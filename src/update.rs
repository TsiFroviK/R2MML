//! App updates are fetched from the official GitHub release and staged outside the app.
use anyhow::Result;
use semver::Version;

pub const CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Debug)]
pub struct Release {
    pub version: Version,
    #[cfg(any(target_os = "macos", test))]
    url: String,
    #[cfg(any(target_os = "macos", test))]
    digest: String,
    #[cfg(any(target_os = "macos", test))]
    size: u64,
}

#[cfg(any(target_os = "macos", test))]
const RELEASE_DOWNLOADS: &str = "https://github.com/TsiFroviK/R2MML/releases/download";
#[cfg(any(target_os = "macos", test))]
const MAX_IMAGE_SIZE: u64 = 256 * 1024 * 1024;

#[cfg(any(target_os = "macos", test))]
#[derive(serde::Deserialize)]
struct GitHubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}

#[cfg(any(target_os = "macos", test))]
#[derive(serde::Deserialize)]
struct GitHubAsset {
    name: String,
    state: String,
    browser_download_url: String,
    digest: Option<String>,
    size: u64,
}

#[cfg(any(target_os = "macos", test))]
fn select_release(bytes: &[u8], current_version: &str) -> Result<Option<Release>> {
    use anyhow::{Context, ensure};

    let latest: GitHubRelease = serde_json::from_slice(bytes)?;
    let version = Version::parse(
        latest
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&latest.tag_name),
    )?;
    if latest.draft
        || latest.prerelease
        || !version.pre.is_empty()
        || !version.build.is_empty()
        || version <= Version::parse(current_version)?
    {
        return Ok(None);
    }
    let filename = format!("R2MML-{version}-macOS-universal.dmg");
    let url = format!("{RELEASE_DOWNLOADS}/{}/{filename}", latest.tag_name);
    let mut candidates = latest
        .assets
        .into_iter()
        .filter(|asset| asset.name == filename);
    let Some(asset) = candidates.next() else {
        // Publishing the tag and uploading the disk image can be separate steps.
        return Ok(None);
    };
    ensure!(
        candidates.next().is_none(),
        "GitHub release has duplicate R2MML disk images"
    );
    if asset.state != "uploaded" {
        return Ok(None);
    }
    ensure!(
        asset.browser_download_url == url,
        "Unexpected GitHub release download URL"
    );
    ensure!(
        (1..=MAX_IMAGE_SIZE).contains(&asset.size),
        "Invalid update size"
    );
    let digest = asset
        .digest
        .as_deref()
        .and_then(|v| v.strip_prefix("sha256:"))
        .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .context("GitHub release does not provide a valid SHA-256 digest")?
        .to_ascii_lowercase();
    Ok(Some(Release {
        version,
        url,
        digest,
        size: asset.size,
    }))
}

#[cfg(target_os = "macos")]
pub use platform::{PreparedUpdate, apply_pending, check, prepare};
#[cfg(not(target_os = "macos"))]
pub use platform::{PreparedUpdate, check, prepare};

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;
    use std::path::Path;

    pub struct PreparedUpdate;

    impl PreparedUpdate {
        pub fn handoff(self, _: &Path) -> Result<()> {
            anyhow::bail!("App updates require macOS")
        }
    }

    pub fn check() -> Result<Option<Release>> {
        Ok(None)
    }

    pub fn prepare(_: &Release, _: impl FnMut(f32)) -> Result<PreparedUpdate> {
        anyhow::bail!("App updates require macOS")
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use anyhow::{Context, bail, ensure};
    use fs2::FileExt;
    use reqwest::{Url, blocking::Client, redirect};
    use sha2::{Digest, Sha256};
    use std::{
        ffi::CString,
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        os::unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, PermissionsExt},
            process::CommandExt,
        },
        path::{Path, PathBuf},
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    const API: &str = "https://api.github.com/repos/TsiFroviK/R2MML/releases/latest";
    const APP_ID: &str = "io.github.tsifrovik.r2mml";
    const EXECUTABLE: &str = "Contents/MacOS/R2MML";
    const STAGING_PREFIX: &str = ".r2mml-update-";
    const STAGED_APP: &str = "R2MML-new.app";

    pub struct PreparedUpdate {
        directory: tempfile::TempDir,
        version: Version,
    }

    fn allowed_url(url: &Url) -> bool {
        url.scheme() == "https"
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && matches!(
                url.host_str(),
                Some(
                    "api.github.com"
                        | "github.com"
                        | "release-assets.githubusercontent.com"
                        | "objects.githubusercontent.com"
                        | "github-releases.githubusercontent.com"
                )
            )
    }

    fn client(timeout: Duration) -> Result<Client> {
        Client::builder()
            .user_agent(concat!("R2MML/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(timeout)
            .redirect(redirect::Policy::custom(|attempt| {
                if allowed_url(attempt.url()) && attempt.previous().len() < 5 {
                    attempt.follow()
                } else {
                    attempt.error("unexpected GitHub redirect")
                }
            }))
            .build()
            .context("Could not prepare the GitHub connection")
    }

    pub fn check() -> Result<Option<Release>> {
        // Running from cargo or a test fixture should not present an install prompt.
        if current_bundle().is_err() {
            return Ok(None);
        }
        let response = client(Duration::from_secs(20))?
            .get(API)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .send()
            .context("Could not check GitHub for updates")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        const MAX_METADATA: u64 = 1024 * 1024;
        response
            .error_for_status()?
            .take(MAX_METADATA + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_METADATA,
            "GitHub release response is too large"
        );
        select_release(&bytes, env!("CARGO_PKG_VERSION"))
    }

    fn current_bundle() -> Result<PathBuf> {
        let exe = std::env::current_exe()?.canonicalize()?;
        let bundle = exe
            .ancestors()
            .nth(3)
            .context("R2MML must run from its application bundle")?;
        ensure!(
            bundle.extension().is_some_and(|ext| ext == "app") && bundle.join(EXECUTABLE) == exe,
            "R2MML must run from its application bundle"
        );
        bundle_metadata(bundle, env!("CARGO_PKG_VERSION"))?;
        Ok(bundle.to_owned())
    }

    fn bundle_metadata(app: &Path, version: &str) -> Result<()> {
        ensure!(
            fs::symlink_metadata(app)?.is_dir(),
            "Invalid R2MML bundle directory"
        );
        let info = plist::Value::from_file(app.join("Contents/Info.plist"))?;
        let info = info
            .as_dictionary()
            .context("Invalid R2MML bundle metadata")?;
        for (key, expected) in [
            ("CFBundleIdentifier", APP_ID),
            ("CFBundleExecutable", "R2MML"),
            ("CFBundlePackageType", "APPL"),
            ("CFBundleShortVersionString", version),
            ("CFBundleVersion", version),
        ] {
            ensure!(
                info.get(key).and_then(plist::Value::as_string) == Some(expected),
                "R2MML application metadata does not match the expected {key}"
            );
        }
        Ok(())
    }

    fn verify_bundle(app: &Path, version: &Version) -> Result<()> {
        bundle_metadata(app, &version.to_string())?;
        let path = app.join(EXECUTABLE);
        ensure!(
            fs::symlink_metadata(&path)?.is_file() && path.canonicalize()? == path,
            "Invalid executable in R2MML update"
        );
        // Read the universal Mach-O header directly. The user does not need Xcode's lipo.
        let mut header = Vec::new();
        File::open(path)?.take(4096).read_to_end(&mut header)?;
        let arch = crate::launch::macho_architectures(&header)?;
        ensure!(
            arch.iter().any(|v| v == "arm64") && arch.iter().any(|v| v == "x86_64"),
            "The update must include both Apple Silicon and Intel executables"
        );
        run(
            Command::new("/usr/bin/codesign")
                .args(["--verify", "--deep", "--strict", "--all-architectures"])
                .arg(app),
            "The update has an invalid macOS code signature",
        )
    }

    fn run(command: &mut Command, context: &str) -> Result<()> {
        let output = command.output().with_context(|| context.to_owned())?;
        ensure!(
            output.status.success(),
            "{context}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }

    pub fn prepare(release: &Release, mut on_progress: impl FnMut(f32)) -> Result<PreparedUpdate> {
        let app = current_bundle()?;
        let parent = app
            .parent()
            .context("R2MML has no installation directory")?;
        let directory = tempfile::Builder::new()
            .prefix(STAGING_PREFIX)
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(parent)
            .context("R2MML cannot write to its installation directory")?;
        ensure!(
            fs::metadata(directory.path())?.mode() & 0o077 == 0,
            "The installation volume does not support a private R2MML update folder"
        );
        let image = directory.path().join("update.dmg");
        let mut file = File::create_new(&image)?;
        let mut response = client(Duration::from_secs(300))?
            .get(&release.url)
            .send()?
            .error_for_status()?;
        ensure!(
            allowed_url(response.url()),
            "Unexpected update response URL"
        );
        if let Some(size) = response.content_length() {
            ensure!(
                size == release.size,
                "GitHub update size changed during download"
            );
        }
        let mut buffer = [0; 64 * 1024];
        let mut digest = Sha256::new();
        let mut bytes = 0;
        let mut last_percent = 0;
        loop {
            let count = response.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            bytes += count as u64;
            ensure!(
                bytes <= release.size,
                "GitHub update exceeded the declared size"
            );
            file.write_all(&buffer[..count])?;
            digest.update(&buffer[..count]);
            let percent = bytes * 100 / release.size;
            if percent != last_percent {
                on_progress(bytes as f32 / release.size as f32);
                last_percent = percent;
            }
        }
        ensure!(
            bytes == release.size && format!("{:x}", digest.finalize()) == release.digest,
            "The downloaded update failed the SHA-256 integrity check"
        );
        file.sync_all()?;
        drop(file);

        let mount = directory.path().join("mounted");
        fs::create_dir(&mount)?;
        if let Err(error) = run(
            Command::new("/usr/bin/hdiutil")
                .args([
                    "attach",
                    "-readonly",
                    "-nobrowse",
                    "-noautoopen",
                    "-mountpoint",
                ])
                .arg(&mount)
                .arg(&image),
            "Could not open the R2MML update disk image",
        ) {
            // If attach partially succeeded, avoid walking the read-only
            // mount during cleanup. The failed attempt never touched the app.
            if fs::read_dir(&mount).is_ok_and(|mut entries| entries.next().is_some())
                && run(
                    Command::new("/usr/bin/hdiutil").arg("detach").arg(&mount),
                    "Could not eject the R2MML update disk image",
                )
                .is_err()
            {
                let _ = directory.keep();
            }
            return Err(error);
        }
        let candidate = directory.path().join(STAGED_APP);
        let staged = (|| -> Result<()> {
            let source = mount.join("R2MML.app");
            verify_bundle(&source, &release.version)?;
            run(
                Command::new("/usr/bin/ditto").arg(source).arg(&candidate),
                "Could not copy the update into the installation directory",
            )?;
            verify_bundle(&candidate, &release.version)
        })();
        if let Err(error) = run(
            Command::new("/usr/bin/hdiutil").arg("detach").arg(&mount),
            "Could not eject the R2MML update disk image",
        ) {
            // A mounted filesystem must not be walked by TempDir's cleanup.
            let _ = directory.keep();
            return Err(error);
        }
        staged?;
        fs::remove_file(image)?;
        Ok(PreparedUpdate {
            directory,
            version: release.version.clone(),
        })
    }

    impl PreparedUpdate {
        pub fn handoff(self, logs: &Path) -> Result<()> {
            let report = OpenOptions::new()
                .create(true)
                .append(true)
                .open(logs.join("Updater.log"))?;
            Command::new(std::env::current_exe()?)
                .arg("--apply-update")
                .arg(self.directory.path())
                .arg(self.version.to_string())
                .arg(std::process::id().to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::from(report.try_clone()?))
                .stderr(Stdio::from(report))
                .process_group(0)
                .spawn()
                .context("Could not start the R2MML update installer")?;
            let _ = self.directory.keep();
            Ok(())
        }
    }

    pub fn apply_pending(args: &[String], profile_root: &Path, logs: &Path) -> Result<()> {
        let [directory, version, parent_pid] = args else {
            bail!("Incomplete R2MML update transaction");
        };
        let app = current_bundle()?;
        let parent_pid: u32 = parent_pid.parse()?;
        ensure!(
            parent_pid > 1 && parent_pid != std::process::id(),
            "Invalid R2MML parent process"
        );

        let result = (|| -> Result<()> {
            wait_for_exit(parent_pid)?;
            let version = Version::parse(version)?;
            ensure!(
                version > Version::parse(env!("CARGO_PKG_VERSION"))?
                    && version.pre.is_empty()
                    && version.build.is_empty(),
                "Invalid target release version"
            );
            let directory = PathBuf::from(directory);
            let metadata = fs::symlink_metadata(&directory)?;
            ensure!(
                metadata.is_dir()
                    && directory.canonicalize()? == directory
                    && directory.parent() == app.parent()
                    && directory
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with(STAGING_PREFIX))
                    && metadata.uid() == fs::metadata(profile_root)?.uid()
                    && metadata.mode() & 0o077 == 0,
                "Invalid R2MML update staging directory"
            );
            let candidate = directory.join(STAGED_APP);
            verify_bundle(&candidate, &version)?;

            // A second manager instance cannot read or migrate the shared
            // profile while the update is switching the application bundle.
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .open(profile_root.join("manager.lock"))?;
            FileExt::try_lock_exclusive(&lock).context("Another instance of R2MML is running")?;
            bundle_metadata(&app, env!("CARGO_PKG_VERSION"))?;
            swap_bundles(&app, &candidate)?;
            drop(lock);
            if let Err(error) = open_app(&app) {
                swap_bundles(&app, &candidate).with_context(|| {
                    format!(
                        "{error:#}. Could not restore the previous version at {}",
                        candidate.display()
                    )
                })?;
                return Err(error);
            }
            // At this point candidate contains the previous bundle, located in
            // the private sibling temporary directory verified above.
            if let Err(error) = fs::remove_dir_all(&directory) {
                eprintln!(
                    "Could not remove previous R2MML update files in {}: {error}",
                    directory.display()
                );
            }
            Ok(())
        })();
        if let Err(error) = &result {
            let _ = crate::catalog::atomic_write(
                &logs.join("UpdateError.txt"),
                format!("{error:#}").as_bytes(),
            );
            let _ = open_app(&app);
        }
        result
    }

    fn wait_for_exit(pid: u32) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if !Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?
                .success()
            {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "R2MML did not close for the update"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn open_app(app: &Path) -> Result<()> {
        run(
            Command::new("/usr/bin/open").arg("-n").arg(app),
            "Could not restart R2MML",
        )
    }

    pub(super) fn swap_bundles(app: &Path, candidate: &Path) -> Result<()> {
        use std::os::raw::{c_char, c_int, c_uint};
        unsafe extern "C" {
            fn renamex_np(from: *const c_char, to: *const c_char, flags: c_uint) -> c_int;
        }
        const RENAME_SWAP: c_uint = 0x00000002;
        let from = CString::new(app.as_os_str().as_bytes())?;
        let to = CString::new(candidate.as_os_str().as_bytes())?;
        // Both owned directories are on the same volume. The swap makes the
        // installed bundle recoverable even if the helper is interrupted.
        let result = unsafe { renamex_np(from.as_ptr(), to.as_ptr(), RENAME_SWAP) };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(22 | 45 | 102)) {
            return Err(error).context("Could not replace the installed R2MML application");
        }
        // Some external filesystems don't support RENAME_SWAP. Keep a sibling
        // backup until the replacement is in place, and restore on failure.
        let backup = candidate.with_file_name("R2MML-previous.app");
        ensure!(
            !backup.exists(),
            "A previous R2MML backup exists at {}",
            backup.display()
        );
        fs::rename(app, &backup).context("Could not back up the installed R2MML application")?;
        if let Err(error) = fs::rename(candidate, app) {
            fs::rename(&backup, app).with_context(|| {
                format!(
                    "Could not restore the previous R2MML version from {} after: {error}",
                    backup.display()
                )
            })?;
            return Err(error).context("Could not install the R2MML update");
        }
        if let Err(error) = fs::rename(&backup, candidate) {
            // Put the original back before reporting failure when the old
            // application cannot be moved to the rollback slot.
            fs::rename(app, candidate)?;
            fs::rename(&backup, app)?;
            return Err(error).context("Could not prepare the R2MML rollback");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn metadata() -> serde_json::Value {
        json!({
            "tag_name": "v0.2.0",
            "draft": false,
            "prerelease": false,
            "assets": [{
                "name": "R2MML-0.2.0-macOS-universal.dmg",
                "state": "uploaded",
                "browser_download_url": format!("{RELEASE_DOWNLOADS}/v0.2.0/R2MML-0.2.0-macOS-universal.dmg"),
                "digest": format!("sha256:{}", "A".repeat(64)),
                "size": 123000,
            }],
        })
    }

    #[test]
    fn chooses_only_a_new_stable_universal_release() {
        let mut release = metadata();
        let encoded = release.to_string();
        let selected = select_release(encoded.as_bytes(), "0.1.7")
            .unwrap()
            .unwrap();
        assert_eq!(selected.version.to_string(), "0.2.0");
        assert_eq!(selected.size, 123000);
        assert_eq!(selected.digest, "a".repeat(64));
        assert!(
            selected
                .url
                .ends_with("v0.2.0/R2MML-0.2.0-macOS-universal.dmg")
        );
        assert!(
            select_release(encoded.as_bytes(), "0.2.0")
                .unwrap()
                .is_none()
        );
        release["prerelease"] = json!(true);
        assert!(
            select_release(release.to_string().as_bytes(), "0.1.7")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rejects_redirected_metadata_and_unsigned_assets() {
        let mut release = metadata();
        release["assets"][0]["browser_download_url"] = json!("https://other.invalid/payload.dmg");
        assert!(select_release(release.to_string().as_bytes(), "0.1.7").is_err());
        let mut release = metadata();
        release["assets"][0]["digest"] = serde_json::Value::Null;
        assert!(select_release(release.to_string().as_bytes(), "0.1.7").is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bundle_exchange_preserves_the_path_and_can_restore_the_original() {
        let fixture = tempfile::tempdir().unwrap();
        let app = fixture.path().join("My renamed R2MML.app");
        let staging = fixture.path().join("staging");
        let candidate = staging.join("R2MML-new.app");
        for (path, label) in [(&app, "current"), (&candidate, "update")] {
            std::fs::create_dir_all(path).unwrap();
            std::fs::write(path.join("test-version.txt"), label).unwrap();
        }
        platform::swap_bundles(&app, &candidate).unwrap();
        assert_eq!(
            std::fs::read_to_string(app.join("test-version.txt")).unwrap(),
            "update"
        );
        assert_eq!(
            std::fs::read_to_string(candidate.join("test-version.txt")).unwrap(),
            "current"
        );
        platform::swap_bundles(&app, &candidate).unwrap();
        assert_eq!(
            std::fs::read_to_string(app.join("test-version.txt")).unwrap(),
            "current"
        );
    }
}
