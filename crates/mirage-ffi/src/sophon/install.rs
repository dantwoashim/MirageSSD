//! Native files are materialized in an absent staging tree. Only the three
//! qualified immutable asset subtrees are exported by the mounted source.
use super::*;

pub const ASSET_PREFIXES: [&str; 3] = [
    "GenshinImpact_Data/StreamingAssets/AssetBundles/blocks/",
    "GenshinImpact_Data/StreamingAssets/AudioAssets/",
    "GenshinImpact_Data/StreamingAssets/VideoAssets/",
];
#[derive(Serialize, Deserialize)]
pub struct Inventory {
    pub native_bytes: u64,
    pub virtual_bytes: u64,
    pub native_files: usize,
}

fn assets(generation: &publisher::Generation) -> Result<Vec<Asset>, MirageStatus> {
    let mut assets = Vec::new();
    let mut names = std::collections::HashSet::new();
    for path in &generation.manifests {
        let file = File::open(path).map_err(|_| MirageStatus::IoError)?;
        let mut bytes = Vec::new();
        file.take(128 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| MirageStatus::IoError)?;
        if bytes.len() > 128 * 1024 * 1024 {
            return Err(MirageStatus::IntegrityFailure);
        }
        let manifest: Manifest =
            serde_json::from_slice(&bytes).map_err(|_| MirageStatus::IntegrityFailure)?;
        for asset in manifest.assets {
            if !valid_path(&asset.name) || !names.insert(asset.name.to_lowercase()) {
                return Err(MirageStatus::IntegrityFailure);
            }
            assets.push(asset);
        }
    }
    Ok(assets)
}
pub fn inventory(generation: &publisher::Generation) -> Result<Inventory, MirageStatus> {
    let mut inventory = Inventory {
        native_bytes: 0,
        virtual_bytes: 0,
        native_files: 0,
    };
    for asset in assets(generation)? {
        if ASSET_PREFIXES
            .iter()
            .any(|prefix| asset.name.starts_with(prefix))
        {
            inventory.virtual_bytes = inventory
                .virtual_bytes
                .checked_add(asset.size)
                .ok_or(MirageStatus::IntegrityFailure)?;
        } else {
            inventory.native_bytes = inventory
                .native_bytes
                .checked_add(asset.size)
                .ok_or(MirageStatus::IntegrityFailure)?;
            inventory.native_files += 1;
        }
    }
    Ok(inventory)
}
pub fn native_paths(generation: &publisher::Generation) -> Result<Vec<PathBuf>, MirageStatus> {
    Ok(assets(generation)?
        .into_iter()
        .filter(|asset| {
            !ASSET_PREFIXES
                .iter()
                .any(|prefix| asset.name.starts_with(prefix))
        })
        .map(|asset| PathBuf::from(asset.name))
        .collect())
}

/// Materialize native executables, libraries and metadata from verified CDN
/// chunks, optionally reusing read-only installation seeds. On failure the
/// staging tree remains incomplete and is never published as installed.
pub fn materialize_native(
    generation: &publisher::Generation,
    target: &Path,
    seed_roots: Vec<PathBuf>,
    cache_root: &Path,
) -> Result<Inventory, MirageStatus> {
    cache::ordinary_path(target)?;
    cache::ordinary_path(cache_root)?;
    if target.exists() {
        return Err(MirageStatus::Conflict);
    }
    let native: Vec<_> = assets(generation)?
        .into_iter()
        .filter(|asset| {
            !ASSET_PREFIXES
                .iter()
                .any(|prefix| asset.name.starts_with(prefix))
        })
        .collect();
    if !native.iter().any(|asset| asset.name == "GenshinImpact.exe") {
        return Err(MirageStatus::IntegrityFailure);
    }
    std::fs::create_dir_all(cache_root).map_err(|_| MirageStatus::IoError)?;
    let config = cache_root.join("native-acquisition.json");
    let config_bytes = serde_json::to_vec(&serde_json::json!({ "manifests": generation.manifests,
        "export_prefixes": [], "export_files": native.iter().map(|asset| &asset.name).collect::<Vec<_>>(),
        "seed_roots": seed_roots, "cache_root": cache_root, "disk_budget_bytes": 0,
        "disk_free_floor_bytes": 0, "memory_budget_bytes": 0 })).map_err(|_| MirageStatus::Internal)?;
    std::fs::write(&config, config_bytes).map_err(|_| MirageStatus::IoError)?;
    let source = SophonSource::open(&config)?;
    std::fs::create_dir(target).map_err(|_| MirageStatus::IoError)?;
    let mut buffer = vec![0; MAX_CHUNK as usize];
    for asset in native {
        let destination = target.join(&asset.name);
        cache::ordinary_path(&destination)?;
        std::fs::create_dir_all(destination.parent().ok_or(MirageStatus::InvalidArgument)?)
            .map_err(|_| MirageStatus::IoError)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|_| MirageStatus::IoError)?;
        let path: Vec<u16> = asset
            .name
            .to_lowercase()
            .replace('/', "\\")
            .encode_utf16()
            .collect();
        let index = source
            .entries
            .get(&path)
            .ok_or(MirageStatus::IntegrityFailure)?
            .index;
        let mut offset = 0u64;
        let mut hash = Md5::new();
        while offset < asset.size {
            let count = source.read(index, offset, &mut buffer)?;
            if count == 0 {
                return Err(MirageStatus::IntegrityFailure);
            }
            file.write_all(&buffer[..count])
                .map_err(|_| MirageStatus::IoError)?;
            hash.update(&buffer[..count]);
            offset += count as u64;
        }
        file.sync_all().map_err(|_| MirageStatus::IoError)?;
        if !format!("{:x}", hash.finalize()).eq_ignore_ascii_case(&asset.md5) {
            return Err(MirageStatus::IntegrityFailure);
        }
    }
    // Publisher configuration is generated only in the new staging tree.
    if !generation
        .version
        .bytes()
        .all(|b| b.is_ascii_digit() || b == b'.')
    {
        return Err(MirageStatus::IntegrityFailure);
    }
    let configuration = format!(
        "[General]\nchannel=1\ncps=mihoyo\ngame_version={}\nsub_channel=0\n",
        generation.version
    );
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target.join("config.ini"))
        .map_err(|_| MirageStatus::IoError)?;
    file.write_all(configuration.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|_| MirageStatus::IoError)?;
    inventory(generation)
}
