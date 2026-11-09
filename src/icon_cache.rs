//! Persistent, asynchronous storage for versioned Thunderstore package icons.
use anyhow::{Context as _, Result, ensure};
use eframe::egui::{
    self,
    load::{Bytes, BytesLoadResult, BytesLoader, BytesPoll, LoadError},
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

const MAX_ICON_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ICON_DIMENSION: u32 = 2048;
const LOADER_ID: &str = egui::generate_loader_id!(IconCache);
type Fetch = Arc<dyn Fn(&str) -> Result<Vec<u8>> + Send + Sync>;
type Entry = Arc<Mutex<BytesLoadResult>>;

pub fn install(ctx: &egui::Context, cache_root: &Path) -> Result<()> {
    if ctx.is_loader_installed(LOADER_ID) {
        return Ok(());
    }
    let client = crate::catalog::client()?;
    let fetch: Fetch = Arc::new(move |uri| {
        let response = client
            .get(uri)
            .timeout(Duration::from_secs(20))
            .send()?
            .error_for_status()?;
        ensure!(
            !response.status().is_redirection(),
            "Icon redirect was rejected"
        );
        ensure!(
            !response
                .content_length()
                .is_some_and(|size| size > MAX_ICON_BYTES),
            "Icon exceeds the size limit"
        );
        read_limited(response)
    });
    // egui tries the most recently installed bytes loader first. This is
    // registered after egui_extras, leaving its loaders as fallbacks.
    ctx.add_bytes_loader(Arc::new(IconCache::new(cache_root.join("Icons"), fetch)?));
    Ok(())
}

fn supports(uri: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(uri) else {
        return false;
    };
    let path = url.path().to_ascii_lowercase();
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url
            .host_str()
            .is_some_and(|host| host == "thunderstore.io" || host.ends_with(".thunderstore.io"))
        && path.contains("/icons/")
        && [".png", ".jpg", ".jpeg"]
            .iter()
            .any(|extension| path.ends_with(extension))
}

fn cache_path(root: &Path, uri: &str) -> PathBuf {
    root.join(format!("{:x}.image", Sha256::digest(uri.as_bytes())))
}

fn read_limited(reader: impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(MAX_ICON_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_ICON_BYTES,
        "Icon exceeds the size limit"
    );
    Ok(bytes)
}

fn validate_icon(bytes: Vec<u8>) -> Result<BytesPoll> {
    ensure!(
        bytes.len() as u64 <= MAX_ICON_BYTES,
        "Icon exceeds the size limit"
    );
    let format = image::guess_format(&bytes).context("Icon is not an image")?;
    let mime = match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        _ => anyhow::bail!("Unsupported package icon format"),
    };
    let mut reader = image::ImageReader::with_format(Cursor::new(&bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_ICON_DIMENSION);
    limits.max_image_height = Some(MAX_ICON_DIMENSION);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().context("Icon image is damaged")?;
    Ok(BytesPoll::Ready {
        size: Some(egui::vec2(image.width() as f32, image.height() as f32)),
        bytes: Bytes::from(bytes),
        mime: Some(mime.into()),
    })
}

fn read_cached(root: &Path, uri: &str) -> Option<BytesPoll> {
    let file = File::open(cache_path(root, uri)).ok()?;
    validate_icon(read_limited(file).ok()?).ok()
}

struct Job {
    uri: String,
    entry: Entry,
    ctx: egui::Context,
}
impl Job {
    fn finish(&self, result: Result<BytesPoll>) {
        *self.entry.lock().unwrap() =
            result.map_err(|error| LoadError::Loading(format!("{error:#}")));
        self.ctx.request_repaint();
    }
}

struct IconCache {
    entries: Mutex<HashMap<String, Entry>>,
    jobs: mpsc::Sender<Job>,
}
impl IconCache {
    fn new(root: PathBuf, fetch: Fetch) -> Result<Self> {
        fs::create_dir_all(&root)?;
        let (network_jobs, network_receiver) = mpsc::channel::<Job>();
        let network_receiver = Arc::new(Mutex::new(network_receiver));
        for index in 0..4 {
            let receiver = network_receiver.clone();
            let root = root.clone();
            let fetch = fetch.clone();
            std::thread::Builder::new()
                .name(format!("icon-download-{index}"))
                .spawn(move || {
                    loop {
                        let job = match receiver.lock().unwrap().recv() {
                            Ok(job) => job,
                            Err(_) => break,
                        };
                        let result = fetch(&job.uri).and_then(validate_icon);
                        if let Ok(BytesPoll::Ready { bytes, .. }) = &result {
                            // A full/read-only disk must not hide an otherwise valid
                            // icon. Failed writes leave no partial cache entries.
                            let _ =
                                crate::catalog::atomic_write(&cache_path(&root, &job.uri), bytes);
                        }
                        job.finish(result);
                    }
                })?;
        }
        let (jobs, receiver) = mpsc::channel::<Job>();
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..2 {
            let receiver = receiver.clone();
            let network_jobs = network_jobs.clone();
            let root = root.clone();
            std::thread::Builder::new()
                .name(format!("icon-cache-{index}"))
                .spawn(move || {
                    loop {
                        let job = match receiver.lock().unwrap().recv() {
                            Ok(job) => job,
                            Err(_) => break,
                        };
                        if let Some(cached) = read_cached(&root, &job.uri) {
                            job.finish(Ok(cached));
                        } else if let Err(error) = network_jobs.send(job) {
                            error
                                .0
                                .finish(Err(anyhow::anyhow!("Icon download worker stopped")));
                        }
                    }
                })?;
        }
        // Disk work has its own queue so an offline network cannot delay
        // icons already available locally. Neither queue runs on the UI thread.
        Ok(Self {
            entries: Mutex::new(HashMap::new()),
            jobs,
        })
    }
}

impl BytesLoader for IconCache {
    fn id(&self) -> &str {
        LOADER_ID
    }

    fn load(&self, ctx: &egui::Context, uri: &str) -> BytesLoadResult {
        if !supports(uri) {
            return Err(LoadError::NotSupported);
        }
        let mut entries = self.entries.lock().unwrap();
        if let Some(entry) = entries.get(uri) {
            return entry.lock().unwrap().clone();
        }
        let entry = Arc::new(Mutex::new(Ok(BytesPoll::Pending { size: None })));
        entries.insert(uri.into(), entry.clone());
        if self
            .jobs
            .send(Job {
                uri: uri.into(),
                entry: entry.clone(),
                ctx: ctx.clone(),
            })
            .is_err()
        {
            *entry.lock().unwrap() = Err(LoadError::Loading("Icon cache worker stopped".into()));
        }
        entry.lock().unwrap().clone()
    }

    fn forget(&self, uri: &str) {
        self.entries.lock().unwrap().remove(uri);
    }

    fn forget_all(&self) {
        // These callbacks release egui memory, not persistent user data.
        self.entries.lock().unwrap().clear();
    }

    fn byte_size(&self) -> usize {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .map(|(uri, entry)| {
                uri.len()
                    + match &*entry.lock().unwrap() {
                        Ok(BytesPoll::Ready { bytes, .. }) => bytes.len(),
                        Err(error) => error.to_string().len(),
                        _ => 0,
                    }
            })
            .sum()
    }

    fn has_pending(&self) -> bool {
        self.entries
            .lock()
            .unwrap()
            .values()
            .any(|entry| matches!(&*entry.lock().unwrap(), Ok(BytesPoll::Pending { .. })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    const ICON: &str = "https://ccdn.thunderstore.io/live/repository/icons/Author-Mod-1.0.0.png";

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut output = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(width, height)
            .write_to(&mut output, image::ImageFormat::Png)
            .unwrap();
        output.into_inner()
    }

    fn wait_for_icon(loader: &IconCache, ctx: &egui::Context, uri: &str) -> BytesLoadResult {
        let start = Instant::now();
        loop {
            let result = loader.load(ctx, uri);
            if !matches!(&result, Ok(BytesPoll::Pending { .. })) {
                return result;
            }
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "Icon worker did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn restart_reuses_valid_disk_icon_without_any_network_request() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = png(4, 4);
        let expected = bytes.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let loader = IconCache::new(
            temp.path().into(),
            Arc::new(move |_| {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(bytes.clone())
            }),
        )
        .unwrap();
        let ctx = egui::Context::default();
        assert!(matches!(
            wait_for_icon(&loader, &ctx, ICON),
            Ok(BytesPoll::Ready { .. })
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(fs::read(cache_path(temp.path(), ICON)).unwrap(), expected);
        drop(loader);
        let offline = IconCache::new(
            temp.path().into(),
            Arc::new(|_| anyhow::bail!("Network must not be used for a cached icon")),
        )
        .unwrap();
        let Ok(BytesPoll::Ready { bytes, .. }) = wait_for_icon(&offline, &ctx, ICON) else {
            panic!("Cached icon was unavailable offline");
        };
        assert_eq!(bytes.as_ref(), expected);
    }

    #[test]
    fn clearing_egui_images_retries_errors_and_preserves_disk_icons() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = png(4, 4);
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let loader = IconCache::new(
            temp.path().into(),
            Arc::new(move |_| {
                if count.fetch_add(1, Ordering::SeqCst) == 0 {
                    anyhow::bail!("Network temporarily offline");
                }
                Ok(bytes.clone())
            }),
        )
        .unwrap();
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        ctx.add_bytes_loader(Arc::new(loader));
        let load = || {
            let start = Instant::now();
            loop {
                let result = ctx.try_load_image(ICON, egui::load::SizeHint::default());
                if !matches!(&result, Ok(egui::load::ImagePoll::Pending { .. })) {
                    return result;
                }
                assert!(start.elapsed() < Duration::from_secs(3));
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        assert!(load().is_err());
        assert!(load().is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        ctx.forget_all_images();
        assert!(matches!(load(), Ok(egui::load::ImagePoll::Ready { .. })));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(cache_path(temp.path(), ICON).is_file());

        ctx.forget_all_images();
        assert!(matches!(load(), Ok(egui::load::ImagePoll::Ready { .. })));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn corrupt_cache_is_replaced_by_a_verified_download() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(cache_path(temp.path(), ICON), b"truncated image").unwrap();
        let bytes = png(2, 2);
        let expected = bytes.clone();
        let loader =
            IconCache::new(temp.path().into(), Arc::new(move |_| Ok(bytes.clone()))).unwrap();
        assert!(matches!(
            wait_for_icon(&loader, &egui::Context::default(), ICON),
            Ok(BytesPoll::Ready { .. })
        ));
        assert_eq!(fs::read(cache_path(temp.path(), ICON)).unwrap(), expected);
    }

    #[test]
    fn invalid_oversized_or_excessive_dimension_images_are_not_cached() {
        for bytes in [
            b"not an image".to_vec(),
            vec![0; MAX_ICON_BYTES as usize + 1],
            png(MAX_ICON_DIMENSION + 1, 1),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let loader =
                IconCache::new(temp.path().into(), Arc::new(move |_| Ok(bytes.clone()))).unwrap();
            assert!(wait_for_icon(&loader, &egui::Context::default(), ICON).is_err());
            assert!(!cache_path(temp.path(), ICON).exists());
        }
        assert!(read_limited(Cursor::new(vec![0; MAX_ICON_BYTES as usize + 1])).is_err());
    }

    #[test]
    fn only_thunderstore_icon_urls_are_intercepted() {
        assert!(supports(ICON));
        for uri in [
            "bytes://logo.png",
            "file:///tmp/icon.png",
            "https://github.com/owner/repo/icons/icon.png",
            "https://thunderstore.io.evil.example/icons/icon.png",
            "http://ccdn.thunderstore.io/icons/icon.png",
            "https://ccdn.thunderstore.io:8443/icons/icon.png",
            "https://user:secret@ccdn.thunderstore.io/icons/icon.png",
            "https://ccdn.thunderstore.io/icons/icon.svg",
            "https://ccdn.thunderstore.io/package.zip",
        ] {
            assert!(!supports(uri), "Unexpected interception: {uri}");
        }
    }

    #[test]
    fn latest_loader_uses_disk_while_embedded_images_keep_their_loader() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(cache_path(temp.path(), ICON), png(2, 2)).unwrap();
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        ctx.include_bytes("bytes://local.png", png(1, 1));
        let loader = IconCache::new(
            temp.path().into(),
            Arc::new(|_| anyhow::bail!("Network must not be used")),
        )
        .unwrap();
        ctx.add_bytes_loader(Arc::new(loader));
        let started = Instant::now();
        loop {
            match ctx.try_load_bytes(ICON) {
                Ok(BytesPoll::Ready { .. }) => break,
                Ok(BytesPoll::Pending { .. }) => {}
                Err(error) => panic!("Disk loader was not selected: {error}"),
            }
            assert!(started.elapsed() < Duration::from_secs(3));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(
            ctx.try_load_bytes("bytes://local.png"),
            Ok(BytesPoll::Ready { .. })
        ));
    }
}
