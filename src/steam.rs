use crate::launch::validate_game;
use keyvalues_parser::{Obj, Value};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

const APP_ID: &str = "632360";
const GAME_DIRECTORY: &str = "Risk of Rain 2";
const GAME_BUNDLE: &str = "Risk of Rain 2.app";
const MAX_VDF_SIZE: u64 = 8 * 1024 * 1024;

pub(crate) fn find_game(steam: &Path) -> Option<PathBuf> {
    // Risk2Mac can be added as a Steam shortcut without an app manifest.
    if let Some(game) = game_in_library(steam) {
        return Some(game);
    }

    let source = read_vdf(&steam.join("steamapps/libraryfolders.vdf"))?;
    let folders = parse_object(&source, "libraryfolders")?;
    folders
        .iter()
        .filter(|(key, _)| !key.is_empty() && key.bytes().all(|byte| byte.is_ascii_digit()))
        .flat_map(|(_, entries)| entries)
        .filter_map(|entry| {
            entry
                .get_str()
                .or_else(|| entry.get_obj()?.get("path")?.first()?.get_str())
        })
        .map(Path::new)
        .filter(|path| path.is_absolute() && *path != steam)
        .find_map(game_in_library)
}

fn game_in_library(library: &Path) -> Option<PathBuf> {
    let steamapps = library.join("steamapps");
    let common = steamapps.join("common");
    let manifest = steamapps.join(format!("appmanifest_{APP_ID}.acf"));
    let registered = read_vdf(&manifest).and_then(|source| {
        let state = parse_object(&source, "AppState")?;
        let app_id = state.get("appid")?.first()?.get_str()?;
        let directory = state.get("installdir")?.first()?.get_str()?;
        let directory_path = Path::new(directory);
        if app_id != APP_ID
            || directory.contains(['\\', '\0'])
            || directory_path.file_name()? != directory
        {
            return None;
        }
        Some(common.join(directory_path).join(GAME_BUNDLE))
    });
    registered
        .into_iter()
        .chain([common.join(GAME_DIRECTORY).join(GAME_BUNDLE)])
        .find_map(|candidate| validate_game(&candidate).ok().map(|game| game.app_path))
}

fn read_vdf(path: &Path) -> Option<String> {
    let metadata = path.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_VDF_SIZE {
        return None;
    }
    let mut text = String::new();
    let size = File::open(path)
        .ok()?
        .take(MAX_VDF_SIZE + 1)
        .read_to_string(&mut text)
        .ok()?;
    (size as u64 <= MAX_VDF_SIZE).then_some(text)
}

fn parse_object<'a>(source: &'a str, root: &str) -> Option<Obj<'a>> {
    let document = keyvalues_parser::parse(source.trim_start_matches('\u{feff}'))
        .ok()?
        .into_vdf();
    match document.value {
        Value::Obj(object) if document.key.eq_ignore_ascii_case(root) => Some(object),
        _ => None,
    }
}
