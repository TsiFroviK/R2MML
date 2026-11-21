//! Find the script assembly names Unity reads from enabled mods' asset bundles.

use super::{PROVIDED_RUNTIME, ProfileState, is_managed_pe, list_files, validate_component};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use unity_bundle_assets::{Bundle, Limits, SerializedFile};

const MONO_SCRIPT: i32 = 115;
const LIMITS: Limits = Limits::DEFAULT
    .with_max_file_size(512 << 20)
    .with_max_decompressed(512 << 20)
    .with_max_objects(1 << 20);

pub(crate) fn discover(
    plugins: &Path,
    state: &ProfileState,
    log: &mut impl Write,
) -> Result<Vec<PathBuf>> {
    ensure!(
        fs::symlink_metadata(plugins)?.is_dir(),
        "Invalid active plugin directory"
    );
    let mut assemblies = Vec::new();
    let mut referenced = BTreeSet::new();

    for package in state
        .installed
        .iter()
        .filter(|p| p.enabled && p.full_name != PROVIDED_RUNTIME)
    {
        validate_component(&package.full_name)?;
        let directory = plugins.join(&package.full_name);
        match fs::symlink_metadata(&directory) {
            Ok(kind) => ensure!(
                kind.is_dir(),
                "Invalid active plugin package directory: {}",
                package.full_name
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("Inspect {}", package.full_name));
            }
        }
        let mut files = Vec::new();
        list_files(plugins, &directory, &mut files)?;
        for relative in files {
            let path = plugins.join(relative);
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("dll"))
            {
                assemblies.push(path);
            } else if is_unity_bundle(&path)? {
                if let Err(error) = read_script_assemblies(&path, &mut referenced) {
                    writeln!(
                        log,
                        "Could not inspect scripts in {}: {error:#}",
                        path.display()
                    )?;
                }
            }
        }
    }

    // These libraries define serialized fields instead of MonoScript objects.
    // MSU uses them for asset references, skins, and animation metadata.
    if referenced.contains("msu.runtime.dll") {
        referenced.extend([
            "r2api.addressables.dll".to_owned(),
            "r2api.skins.dll".to_owned(),
            "r2api.animations.runtime.dll".to_owned(),
        ]);
    }
    let mut selected = Vec::new();
    for path in assemblies {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| referenced.contains(&name.to_lowercase()))
            && is_managed_pe(&path)?
        {
            selected.push(path);
        }
    }
    selected.sort_unstable();
    Ok(selected)
}

fn is_unity_bundle(path: &Path) -> io::Result<bool> {
    let mut header = [0; 8];
    match File::open(path)?.read_exact(&mut header) {
        Ok(()) => Ok(unity_bundle_assets::is_bundle(&header)),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error),
    }
}

fn read_script_assemblies(path: &Path, referenced: &mut BTreeSet<String>) -> Result<()> {
    let bundle = Bundle::open_with(path, LIMITS)?;
    for entry in bundle.serialized_files() {
        let bytes = bundle
            .bytes(entry)
            .context("Invalid serialized file range")?;
        let file = SerializedFile::parse_with(bytes.to_vec(), LIMITS)?;
        for script in file
            .objects()
            .iter()
            .filter(|object| object.class_id() == MONO_SCRIPT)
        {
            if let Some(name) = file
                .bytes(script)
                .and_then(|bytes| assembly_name(bytes, file.big_endian()))
            {
                let mut name = name.to_lowercase();
                if !name.ends_with(".dll") {
                    name.push_str(".dll");
                }
                referenced.insert(name);
            }
        }
    }
    Ok(())
}

fn assembly_name(mut data: &[u8], big_endian: bool) -> Option<&str> {
    fn aligned_string<'a>(data: &mut &'a [u8], big_endian: bool) -> Option<&'a str> {
        let bytes = data.get(..4)?.try_into().ok()?;
        let len = if big_endian {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        } as usize;
        let content = data.get(4..4 + len.min(4096))?;
        if len > 4096 {
            return None;
        }
        *data = data.get(4 + len.next_multiple_of(4)..)?;
        str::from_utf8(content).ok()
    }

    aligned_string(&mut data, big_endian)?; // Object name
    data = data.get(20..)?; // Execution order and serialized properties hash
    aligned_string(&mut data, big_endian)?; // CLR class
    aligned_string(&mut data, big_endian)?; // CLR namespace
    let name = aligned_string(&mut data, big_endian)?;
    (!name.is_empty()
        && name
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || b"._-".contains(&ch)))
    .then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_serialized_script_assembly_in_both_byte_orders() {
        for big_endian in [false, true] {
            let mut data = Vec::new();
            for string in ["Example", "Example", "Test.Mod", "Example-Mod.dll"] {
                let len = string.len() as u32;
                data.extend(if big_endian {
                    len.to_be_bytes()
                } else {
                    len.to_le_bytes()
                });
                data.extend(string.as_bytes());
                data.resize(data.len().next_multiple_of(4), 0);
                if data.len() == 12 {
                    data.extend([0; 20]);
                }
            }
            assert_eq!(assembly_name(&data, big_endian), Some("Example-Mod.dll"));
            assert_eq!(assembly_name(&data[..data.len() - 2], big_endian), None);
        }
    }
}
