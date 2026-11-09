//! Native packaging tool. No scripting runtime is required to build a release.
use anyhow::{Context, Result, bail};
use image::{ImageBuffer, Rgba};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("Run {program}"))?;
    if !status.success() {
        bail!("{program} exited with {status}")
    }
    Ok(())
}
fn main() -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("Package R2MML on macOS with Xcode Command Line Tools")
    }
    let universal = std::env::args().any(|s| s == "--universal");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::env::set_current_dir(&root)?;
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let arches: Vec<&str> = if universal {
        vec!["aarch64-apple-darwin", "x86_64-apple-darwin"]
    } else if cfg!(target_arch = "aarch64") {
        vec!["aarch64-apple-darwin"]
    } else {
        vec!["x86_64-apple-darwin"]
    };
    for arch in &arches {
        run("rustup", &["target", "add", arch])?;
        let status = Command::new("cargo")
            .args([
                "build",
                "--release",
                "--locked",
                "--bin",
                "r2mml",
                "--target",
                arch,
            ])
            .env("MACOSX_DEPLOYMENT_TARGET", "11.0")
            .status()?;
        if !status.success() {
            bail!("Release build failed for {arch}")
        }
    }
    let version = env!("CARGO_PKG_VERSION");
    let suffix = if universal {
        "universal"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x86_64"
    };
    let output = root.join("dist").join(format!("R2MML-{version}-{suffix}"));
    if output.exists() {
        bail!(
            "Output already exists; preserve or rename it before packaging: {}",
            output.display()
        )
    }
    let app = output.join("R2MML.app");
    let contents = app.join("Contents");
    let resources = contents.join("Resources");
    let bin = contents.join("MacOS");
    fs::create_dir_all(&resources)?;
    fs::create_dir_all(&bin)?;
    let executable = bin.join("R2MML");
    if universal {
        let args: Vec<String> = vec![
            "-create".into(),
            target
                .join(arches[0])
                .join("release/r2mml")
                .to_string_lossy()
                .into_owned(),
            target
                .join(arches[1])
                .join("release/r2mml")
                .to_string_lossy()
                .into_owned(),
            "-output".into(),
            executable.to_string_lossy().into_owned(),
        ];
        run(
            "/usr/bin/lipo",
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
        )?;
    } else {
        fs::copy(target.join(arches[0]).join("release/r2mml"), &executable)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
    }
    let mut info = plist::Dictionary::new();
    for (key, value) in [
        ("CFBundleName", "R2MML"),
        ("CFBundleDisplayName", "R2MML"),
        ("CFBundleExecutable", "R2MML"),
        ("CFBundleIdentifier", "io.github.tsifrovik.r2mml"),
        ("CFBundlePackageType", "APPL"),
        ("CFBundleIconFile", "R2MML"),
        ("CFBundleShortVersionString", version),
        ("CFBundleVersion", version),
        ("LSMinimumSystemVersion", "11.0"),
        (
            "NSHumanReadableCopyright",
            "Copyright © 2026 R2MML contributors. MIT license.",
        ),
    ] {
        info.insert(key.into(), plist::Value::String(value.into()));
    }
    info.insert(
        "NSHighResolutionCapable".into(),
        plist::Value::Boolean(true),
    );
    info.insert(
        "CFBundleLocalizations".into(),
        plist::Value::Array(vec![
            plist::Value::String("en".into()),
            plist::Value::String("ru".into()),
        ]),
    );
    plist::Value::Dictionary(info).to_file_xml(contents.join("Info.plist"))?;
    icon(&output, &resources)?;
    for file in ["LICENSE", "README.md", "README.ru.md", "THIRD_PARTY.md"] {
        fs::copy(root.join(file), resources.join(file))?;
    }
    notices(&resources, &arches)?;
    run(
        "/usr/bin/codesign",
        &[
            "--force",
            "--sign",
            "-",
            app.to_str().context("Non-Unicode package path")?,
        ],
    )?;
    run(
        "/usr/bin/codesign",
        &["--verify", "--deep", "--strict", app.to_str().unwrap()],
    )?;
    let archive = root
        .join("dist")
        .join(format!("R2MML-{version}-macOS-{suffix}.zip"));
    if archive.exists() {
        bail!("Archive already exists: {}", archive.display())
    }
    run(
        "/usr/bin/ditto",
        &[
            "-c",
            "-k",
            "--sequesterRsrc",
            "--keepParent",
            app.to_str().unwrap(),
            archive.to_str().unwrap(),
        ],
    )?;
    println!(
        "Application: {}\nArchive: {}",
        app.display(),
        archive.display()
    );
    Ok(())
}
fn icon(output: &Path, resources: &Path) -> Result<()> {
    let set = output.join("R2MML.iconset");
    fs::create_dir_all(&set)?;
    let source: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_fn(1024, 1024, |x, y| {
        let (x, y) = (x as f32 / 1024.0, y as f32 / 1024.0);
        let corner = 0.22;
        let dx = (x.clamp(corner, 1.0 - corner) - x).abs();
        let dy = (y.clamp(corner, 1.0 - corner) - y).abs();
        if dx * dx + dy * dy > corner * corner {
            return Rgba([0, 0, 0, 0]);
        }
        let glow = (1.0 - ((x - 0.25).powi(2) + (y - 0.15).powi(2)).sqrt()).max(0.0);
        let mut color = [
            (18.0 + 30.0 * glow) as u8,
            (73.0 + 83.0 * glow) as u8,
            (170.0 + 65.0 * glow) as u8,
            255,
        ];
        // Two original interlocking module shapes; no game artwork or third-party marks.
        let rounded = |cx: f32, cy: f32, w: f32, h: f32, r: f32| {
            let px = (x - cx).abs() - w * 0.5 + r;
            let py = (y - cy).abs() - h * 0.5 + r;
            px.max(0.0).powi(2) + py.max(0.0).powi(2) <= r * r && px <= r && py <= r
        };
        let first =
            rounded(0.40, 0.43, 0.36, 0.40, 0.09) && !rounded(0.40, 0.43, 0.17, 0.21, 0.025);
        let second =
            rounded(0.61, 0.60, 0.36, 0.40, 0.09) && !rounded(0.61, 0.60, 0.17, 0.21, 0.025);
        if first {
            color = [235, 245, 255, 255];
        }
        if second {
            color = [128, 245, 224, 255];
        }
        Rgba(color)
    });
    for size in [16u32, 32, 128, 256, 512] {
        for scale in [1u32, 2] {
            let filename = if scale == 1 {
                format!("icon_{size}x{size}.png")
            } else {
                format!("icon_{size}x{size}@2x.png")
            };
            image::imageops::resize(
                &source,
                size * scale,
                size * scale,
                image::imageops::FilterType::Lanczos3,
            )
            .save(set.join(filename))?;
        }
    }
    run(
        "/usr/bin/iconutil",
        &[
            "-c",
            "icns",
            set.to_str().unwrap(),
            "-o",
            resources.join("R2MML.icns").to_str().unwrap(),
        ],
    )?;
    Ok(())
}
fn notices(resources: &Path, arches: &[&str]) -> Result<()> {
    let mut packages = std::collections::BTreeMap::new();
    for arch in arches {
        let result = Command::new("cargo")
            .args([
                "metadata",
                "--format-version",
                "1",
                "--locked",
                "--filter-platform",
                arch,
            ])
            .output()?;
        if !result.status.success() {
            bail!(
                "Cannot collect dependency license information: {}",
                String::from_utf8_lossy(&result.stderr)
            )
        }
        let data: serde_json::Value = serde_json::from_slice(&result.stdout)?;
        for package in data["packages"]
            .as_array()
            .context("No Cargo package metadata")?
        {
            packages.insert(
                package["id"]
                    .as_str()
                    .context("No Cargo package ID")?
                    .to_owned(),
                package.clone(),
            );
        }
    }
    let mut output = String::from(
        "R2MML dependency notices\nGenerated from the locked Cargo dependency graph.\n\n",
    );
    for package in packages.values() {
        if package["name"] == "r2mml" {
            continue;
        }
        let manifest = PathBuf::from(
            package["manifest_path"]
                .as_str()
                .context("Missing manifest path")?,
        );
        let folder = manifest.parent().unwrap();
        output.push_str(&format!(
            "\n=== {} {} ===\nLicense: {}\nSource: {}\n",
            package["name"].as_str().unwrap_or(""),
            package["version"].as_str().unwrap_or(""),
            package["license"].as_str().unwrap_or("see license files"),
            package["repository"]
                .as_str()
                .unwrap_or("https://crates.io")
        ));
        for entry in fs::read_dir(folder)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if entry.file_type()?.is_file()
                && (name.starts_with("license")
                    || name.starts_with("licence")
                    || name.starts_with("copying")
                    || name == "notice")
                && entry.metadata()?.len() < 500_000
            {
                output.push_str(&format!(
                    "\n--- {} ---\n",
                    entry.file_name().to_string_lossy()
                ));
                output.push_str(
                    &fs::read_to_string(entry.path())
                        .unwrap_or_else(|_| "[Binary license resource; consult upstream.]".into()),
                );
                output.push('\n');
            }
        }
        // These fonts are embedded by egui and carry licenses independent of the crate.
        if package["name"] == "epaint_default_fonts" {
            for entry in fs::read_dir(folder.join("fonts"))? {
                let entry = entry?;
                if entry.path().extension().is_some_and(|e| e == "txt") {
                    output.push_str(&format!(
                        "\n--- fonts/{} ---\n",
                        entry.file_name().to_string_lossy()
                    ));
                    output.push_str(&fs::read_to_string(entry.path())?);
                    output.push('\n');
                }
            }
        }
    }
    fs::write(resources.join("DEPENDENCY-LICENSES.txt"), output)?;
    Ok(())
}
