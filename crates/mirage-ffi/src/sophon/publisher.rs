//! Bounded acquisition of the global Genshin generation from publisher HTTPS.
//! This is independent of operator-supplied source configurations. Endpoint
//! allowlists, transport authentication and publisher checksums establish the
//! acquisition provenance; MD5 alone is not a signature.
use super::*;
use serde_json::Value;

const BRANCHES: &str = "https://sg-hyp-api.hoyoverse.com/hyp/hyp-connect/api/getGameBranches?launcher_id=VYTpXlbWo8&game_ids[]=gopR6Cufr3";
const BUILD: &str = "https://sg-public-api.hoyoverse.com/downloader/sophon_chunk/api/getBuild";
const MAX_JSON: u64 = 4 * 1024 * 1024;
const MAX_MANIFEST: u64 = 64 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub struct Generation {
    pub version: String,
    pub build_id: String,
    pub language: String,
    pub manifests: Vec<PathBuf>,
    pub logical_bytes: u64,
}

fn invalid() -> MirageStatus {
    MirageStatus::IntegrityFailure
}
fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, MirageStatus> {
    value[field]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 2048)
        .ok_or_else(invalid)
}
fn number(value: &Value, field: &str) -> Result<u64, MirageStatus> {
    value[field]
        .as_u64()
        .or_else(|| value[field].as_str()?.parse().ok())
        .ok_or_else(invalid)
}
fn download(
    client: &reqwest::blocking::Client,
    url: reqwest::Url,
    max: u64,
) -> Result<Vec<u8>, MirageStatus> {
    let response = client
        .get(url)
        .header("User-Agent", "HYPLauncher/1.0")
        .header("Accept-Encoding", "identity")
        .send()
        .map_err(|_| MirageStatus::BackendUnavailable)?;
    if response.status() != reqwest::StatusCode::OK
        || response.content_length().is_some_and(|n| n > max)
        || response
            .headers()
            .get("content-encoding")
            .is_some_and(|v| v != "identity")
    {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    response
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| MirageStatus::BackendUnavailable)?;
    if bytes.len() as u64 > max {
        return Err(invalid());
    }
    Ok(bytes)
}
fn api(client: &reqwest::blocking::Client, url: reqwest::Url) -> Result<Value, MirageStatus> {
    let value: Value =
        serde_json::from_slice(&download(client, url, MAX_JSON)?).map_err(|_| invalid())?;
    if value["retcode"].as_i64() != Some(0) {
        return Err(MirageStatus::BackendUnavailable);
    }
    Ok(value["data"].clone())
}
fn cdn_url(prefix: &str, id: Option<&str>) -> Result<reqwest::Url, MirageStatus> {
    let mut url = reqwest::Url::parse(prefix).map_err(|_| invalid())?;
    if url.scheme() != "https"
        || url.host_str() != Some("autopatchhk.yuanshen.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().starts_with("/client_app/sophon/")
    {
        return Err(invalid());
    }
    if let Some(id) = id {
        if !crate::safe_object_id(id) {
            return Err(invalid());
        }
        url.set_path(&format!("{}/{id}", url.path().trim_end_matches('/')));
    }
    Ok(url)
}

/// Acquire into a new, empty generation directory. Never replace an active
/// generation; the caller publishes a validated pointer after preparation.
pub fn acquire(directory: &Path, language: &str) -> Result<Generation, MirageStatus> {
    if !["en-us", "ja-jp", "ko-kr", "zh-cn"].contains(&language) {
        return Err(MirageStatus::InvalidArgument);
    }
    let directory = if directory.is_absolute() {
        directory.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| MirageStatus::IoError)?
            .join(directory)
    };
    cache::ordinary_path(&directory)?;
    std::fs::create_dir(&directory).map_err(|_| MirageStatus::Conflict)?;
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|_| MirageStatus::BackendUnavailable)?;
    let branches = api(
        &client,
        reqwest::Url::parse(BRANCHES).map_err(|_| invalid())?,
    )?;
    let branch = branches["game_branches"]
        .as_array()
        .and_then(|v| v.iter().find(|v| v["game"]["biz"] == "hk4e_global"))
        .ok_or_else(invalid)?;
    let main = &branch["main"];
    let mut url = reqwest::Url::parse(BUILD).map_err(|_| invalid())?;
    url.query_pairs_mut()
        .append_pair("branch", text(main, "branch")?)
        .append_pair("package_id", text(main, "package_id")?)
        .append_pair("password", main["password"].as_str().ok_or_else(invalid)?);
    let build = api(&client, url)?;
    let mut generation = Generation {
        version: text(main, "tag")?.into(),
        build_id: text(&build, "build_id")?.into(),
        language: language.into(),
        manifests: Vec::new(),
        logical_bytes: 0,
    };
    let mut names = std::collections::HashSet::new();
    for field in ["game", language] {
        let descriptors = build["manifests"].as_array().ok_or_else(invalid)?;
        let matching: Vec<_> = descriptors
            .iter()
            .filter(|m| m["matching_field"] == field)
            .collect();
        if matching.len() != 1 {
            return Err(invalid());
        }
        let descriptor = matching[0];
        for kind in ["manifest_download", "chunk_download"] {
            if number(&descriptor[kind], "compression")? != 1
                || number(&descriptor[kind], "encryption")? != 0
                || descriptor[kind]["url_suffix"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
            {
                return Err(invalid());
            }
        }
        let metadata = &descriptor["manifest"];
        let size = number(metadata, "compressed_size")?;
        let decoded_size = number(metadata, "uncompressed_size")?;
        if size > MAX_MANIFEST || decoded_size > MAX_MANIFEST {
            return Err(invalid());
        }
        let url = cdn_url(
            text(&descriptor["manifest_download"], "url_prefix")?,
            Some(text(metadata, "id")?),
        )?;
        let compressed = download(&client, url, size)?;
        if compressed.len() as u64 != size {
            return Err(invalid());
        }
        let mut decoder =
            zstd::stream::read::Decoder::new(&compressed[..]).map_err(|_| invalid())?;
        decoder.window_log_max(26).map_err(|_| invalid())?;
        let mut raw = Vec::new();
        decoder
            .take(decoded_size + 1)
            .read_to_end(&mut raw)
            .map_err(|_| invalid())?;
        if raw.len() as u64 != decoded_size
            || !format!("{:x}", Md5::digest(&raw)).eq_ignore_ascii_case(text(metadata, "checksum")?)
        {
            return Err(invalid());
        }
        let prefix = cdn_url(text(&descriptor["chunk_download"], "url_prefix")?, None)?.to_string();
        let assets = decode(&raw)?;
        for asset in &assets {
            if !names.insert(asset.name.to_lowercase()) {
                return Err(invalid());
            }
            generation.logical_bytes = generation
                .logical_bytes
                .checked_add(asset.size)
                .ok_or_else(invalid)?;
        }
        let path = directory.join(format!("manifest_{field}.json"));
        let bytes = serde_json::to_vec(&Manifest {
            chunk_url_prefix: prefix,
            assets,
        })
        .map_err(|_| invalid())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| MirageStatus::IoError)?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| MirageStatus::IoError)?;
        generation.manifests.push(path);
    }
    let bytes = serde_json::to_vec_pretty(&generation).map_err(|_| invalid())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("generation.json"))
        .map_err(|_| MirageStatus::IoError)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| MirageStatus::IoError)?;
    Ok(generation)
}

enum Field<'a> {
    Integer(u64),
    Bytes(&'a [u8]),
}
fn varint(bytes: &[u8], cursor: &mut usize) -> Result<u64, MirageStatus> {
    let mut value = 0;
    for shift in (0..70).step_by(7) {
        let byte = *bytes.get(*cursor).ok_or_else(invalid)?;
        *cursor += 1;
        if shift == 63 && byte > 1 {
            return Err(invalid());
        }
        value |= u64::from(byte & 127) << shift;
        if byte < 128 {
            return Ok(value);
        }
    }
    Err(invalid())
}
fn fields(bytes: &[u8]) -> Result<Vec<(u64, Field<'_>)>, MirageStatus> {
    let mut cursor = 0;
    let mut result = Vec::new();
    while cursor < bytes.len() {
        if result.len() >= 1_000_000 {
            return Err(invalid());
        }
        let key = varint(bytes, &mut cursor)?;
        if key >> 3 == 0 {
            return Err(invalid());
        }
        let value = match key & 7 {
            0 => Field::Integer(varint(bytes, &mut cursor)?),
            wire @ (1 | 2 | 5) => {
                let length = match wire {
                    1 => 8,
                    5 => 4,
                    _ => usize::try_from(varint(bytes, &mut cursor)?).map_err(|_| invalid())?,
                };
                let end = cursor.checked_add(length).ok_or_else(invalid)?;
                let value = bytes.get(cursor..end).ok_or_else(invalid)?;
                cursor = end;
                Field::Bytes(value)
            }
            _ => return Err(invalid()),
        };
        result.push((key >> 3, value));
    }
    Ok(result)
}
fn integer(fields: &[(u64, Field<'_>)], id: u64) -> Result<u64, MirageStatus> {
    let found: Vec<_> = fields.iter().filter(|(n, _)| *n == id).collect();
    match found.as_slice() {
        [] => Ok(0),
        [(_, Field::Integer(n))] => Ok(*n),
        _ => Err(invalid()),
    }
}
fn string(fields: &[(u64, Field<'_>)], id: u64) -> Result<String, MirageStatus> {
    let found: Vec<_> = fields.iter().filter(|(n, _)| *n == id).collect();
    match found.as_slice() {
        [(_, Field::Bytes(b))] => std::str::from_utf8(b)
            .map(str::to_string)
            .map_err(|_| invalid()),
        _ => Err(invalid()),
    }
}
fn decode(raw: &[u8]) -> Result<Vec<Asset>, MirageStatus> {
    let mut assets = Vec::new();
    for (id, value) in fields(raw)? {
        if id != 1 {
            continue;
        }
        let Field::Bytes(bytes) = value else {
            return Err(invalid());
        };
        let record = fields(bytes)?;
        // Publisher directory records are not files.
        if integer(&record, 3)? != 0 {
            continue;
        }
        let mut asset = Asset {
            name: string(&record, 1)?,
            size: integer(&record, 4)?,
            md5: string(&record, 5)?,
            chunks: Vec::new(),
            prefix: String::new(),
        };
        if !valid_path(&asset.name) || !valid_md5(&asset.md5) {
            return Err(invalid());
        }
        for (id, value) in &record {
            if *id != 2 {
                continue;
            }
            let Field::Bytes(bytes) = value else {
                return Err(invalid());
            };
            let c = fields(bytes)?;
            let chunk = Chunk {
                name: string(&c, 1)?,
                md5: string(&c, 2)?,
                offset: integer(&c, 3)?,
                size: integer(&c, 4)?,
                dsize: integer(&c, 5)?,
            };
            if !crate::safe_object_id(&chunk.name)
                || !chunk
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || !valid_md5(&chunk.md5)
                || chunk.size == 0
                || chunk.size > MAX_COMPRESSED
                || chunk.dsize == 0
                || chunk.dsize > MAX_CHUNK
            {
                return Err(invalid());
            }
            asset.chunks.push(chunk);
        }
        asset.chunks.sort_by_key(|c| c.offset);
        let mut end = 0u64;
        for chunk in &asset.chunks {
            if chunk.offset != end {
                return Err(invalid());
            }
            end = end.checked_add(chunk.dsize).ok_or_else(invalid)?;
        }
        if end != asset.size {
            return Err(invalid());
        }
        assets.push(asset);
    }
    if assets.is_empty() {
        return Err(invalid());
    }
    Ok(assets)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protobuf_rejects_truncation_overflow_and_bad_wire_types() {
        for bytes in [
            &[10, 5, 1][..],
            &[8, 128][..],
            &[15][..],
            &[0][..],
            &[255; 11][..],
        ] {
            assert!(fields(bytes).is_err());
        }
    }
    #[test]
    fn official_cdn_allowlist_rejects_credentials_subdomains_and_traversal() {
        for url in [
            "http://autopatchhk.yuanshen.com/client_app/sophon/x",
            "https://autopatchhk.yuanshen.com.evil.invalid/client_app/sophon/x",
            "https://u@autopatchhk.yuanshen.com/client_app/sophon/x",
            "https://autopatchhk.yuanshen.com/client_app/sophon/../../x",
        ] {
            assert!(cdn_url(url, None).is_err());
        }
        assert!(
            cdn_url(
                "https://autopatchhk.yuanshen.com/client_app/sophon/chunks/x",
                Some("../x")
            )
            .is_err()
        );
    }
}
