//! Compatibility adapters for unmodified Thunderstore downloads.
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) const CORE: &str = "RiskofThunder-R2API_Core";
pub(super) const PACK: &str = "RiskofThunder-RoR2BepInExPack";
pub(super) const PACK_DEPENDENCY: &str = "RiskofThunder-RoR2BepInExPack-1.50.0";
pub(super) const ADAPTER: &str = "ror2bepinexpack-darwin-1";
pub(super) const DLL: &str = "plugins/RoR2BepInExPack/RoR2BepInExPack.dll";

struct Recipe {
    version: &'static str,
    digest: &'static str,
    adapted_digest: &'static str,
    changes: &'static [Change],
}

struct Change {
    offset: usize,
    original: &'static [u8],
    replacement: &'static [u8],
}

// New IL body for LegacyShaderDetours.OnShaderFind. Try Risk of Rain 2's
// legacy catalog, then call the original Unity method if it returns no
// shader or throws for a shader that is not in the catalog (e.g. UI/Default).
// The catch clause uses the assembly's existing System.Exception TypeRef.
const SHADER_FALLBACK: &[u8; 64] = &[
    0x1b, 0x30, 0x02, 0x00, 0x22, 0x00, 0x00, 0x00, 0x39, 0x00, 0x00, 0x11, 0x02, 0x28, 0x05, 0x02,
    0x00, 0x0a, 0x0a, 0xde, 0x03, 0x26, 0xde, 0x00, 0x06, 0x28, 0x55, 0x01, 0x00, 0x0a, 0x2c, 0x02,
    0x06, 0x2a, 0x7e, 0x40, 0x00, 0x00, 0x04, 0x02, 0x6f, 0xe4, 0x1b, 0x00, 0x06, 0x2a, 0x00, 0x00,
    0x01, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x09, 0x09, 0x00, 0x03, 0x18, 0x00, 0x00, 0x01,
];

const RECIPES: &[Recipe] = &[Recipe {
    version: "1.50.0",
    digest: "917d4b1eb3a4d0ab7efebb7c378d3aacb28e88985a3adbbba335190c8470f811",
    adapted_digest: "2252ead80402dadfc240d764301ff14fe3bc0f4838c5bf55837c6f3eec6291f3",
    changes: &[
        Change {
            // Awake calls a Windows-only Mono stack trace diagnostic.
            // Replace the static void call with same-size IL NOPs.
            offset: 0xa75,
            original: &[0x28, 0xe2, 0x00, 0x00, 0x06],
            replacement: &[0; 5],
        },
        Change {
            // Extend .text VirtualSize into its existing aligned raw padding.
            offset: 0x180,
            original: &0x00c6_80ec_u32.to_le_bytes(),
            replacement: &0x00c6_812c_u32.to_le_bytes(),
        },
        Change {
            // Point the OnShaderFind MethodDef RVA at the replacement body.
            offset: 0x30f068,
            original: &0x0000_67dc_u32.to_le_bytes(),
            replacement: &0x00c6_a0ec_u32.to_le_bytes(),
        },
        Change {
            offset: 0xc682ec,
            original: &[0; 64],
            replacement: SHADER_FALLBACK,
        },
    ],
}];

pub(super) fn add_dependencies(name: &str, version: &str, dependencies: &mut Vec<String>) {
    // R2API Core 5.1.6+ declares this as a hard BepInDependency within its
    // assembly. The Thunderstore manifest expects the Windows distribution
    // to supply it transitively, but R2MML installs native BepInEx instead.
    if name == CORE
        && semver::Version::parse(version).is_ok_and(|v| v >= semver::Version::new(5, 1, 6))
        && !dependencies.iter().any(|d| d == PACK_DEPENDENCY)
    {
        dependencies.push(PACK_DEPENDENCY.into());
    }
}

pub(super) fn can_adapt(name: &str, version: &str) -> bool {
    name != PACK || RECIPES.iter().any(|recipe| recipe.version == version)
}

pub(super) fn prepare_pack(profile: &Path, source: &Path, version: &str) -> Result<PathBuf> {
    let recipe = RECIPES
        .iter()
        .find(|recipe| recipe.version == version)
        .with_context(|| format!("RoR2BepInExPack {version} has no verified macOS adapter"))?;
    let original = source.join(DLL);
    ensure!(
        original.symlink_metadata().is_ok_and(|m| m.is_file()),
        "RoR2BepInExPack DLL is missing"
    );
    let mut bytes = fs::read(&original)?;
    ensure!(
        digest(&bytes) == recipe.digest,
        "RoR2BepInExPack {version} DLL does not match the verified macOS adapter"
    );

    for change in recipe.changes {
        let span = bytes
            .get_mut(change.offset..change.offset + change.original.len())
            .context("Truncated RoR2BepInExPack assembly")?;
        ensure!(
            span == change.original,
            "Unexpected RoR2BepInExPack byte sequence"
        );
        span.copy_from_slice(change.replacement);
    }
    ensure!(
        digest(&bytes) == recipe.adapted_digest,
        "RoR2BepInExPack adaptation checksum mismatch"
    );

    let root = profile.join("macos-compatibility");
    let package = root.join(format!("{PACK}-{version}"));
    for directory in [&root, &package] {
        ensure!(
            !directory.is_symlink(),
            "Compatibility directory must not be a symbolic link: {}",
            directory.display()
        );
    }
    let adapted = package.join("RoR2BepInExPack.dll");
    ensure!(
        !adapted.is_symlink(),
        "Adapted assembly must not be a symbolic link"
    );
    if !fs::read(&adapted).is_ok_and(|old| old == bytes) {
        crate::catalog::atomic_write(&adapted, &bytes)?;
    }
    Ok(adapted)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
