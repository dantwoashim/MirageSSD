use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RootFilter {
    root: PathBuf,
    #[cfg(windows)]
    device_mapping: Option<(String, String)>,
}
impl RootFilter {
    pub fn new(root: &Path) -> std::io::Result<Self> {
        let root = root.canonicalize()?;
        Ok(Self {
            #[cfg(windows)]
            device_mapping: device_mapping(&root)?,
            root,
        })
    }
    pub fn include(&self, path: &Path) -> Option<PathBuf> {
        let absolute = path.canonicalize().ok();
        #[cfg(windows)]
        let absolute = absolute.or_else(|| self.translate_kernel_path(path)?.canonicalize().ok());
        let absolute = absolute?;
        absolute
            .strip_prefix(&self.root)
            .ok()
            .map(Path::to_path_buf)
    }

    #[cfg(windows)]
    fn translate_kernel_path(&self, path: &Path) -> Option<PathBuf> {
        let value = path.as_os_str().to_string_lossy().replace('/', "\\");
        if let Some(value) = value.strip_prefix("\\??\\") {
            return Some(PathBuf::from(value));
        }
        let (device, drive) = self.device_mapping.as_ref()?;
        if value.len() < device.len() || !value[..device.len()].eq_ignore_ascii_case(device) {
            return None;
        }
        let suffix = &value[device.len()..];
        if !suffix.is_empty() && !suffix.starts_with('\\') {
            return None;
        }
        Some(PathBuf::from(format!("{drive}{suffix}")))
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn device_mapping(root: &Path) -> std::io::Result<Option<(String, String)>> {
    use std::path::{Component, Prefix};
    use windows_sys::Win32::Storage::FileSystem::QueryDosDeviceW;

    let drive = match root.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                format!("{}:", char::from(letter))
            }
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    let wide = drive.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut target = vec![0_u16; 32_768];
    // SAFETY: both buffers are valid, writable where required, and explicitly bounded.
    let written = unsafe {
        QueryDosDeviceW(
            wide.as_ptr(),
            target.as_mut_ptr(),
            u32::try_from(target.len()).expect("fixed QueryDosDevice buffer fits u32"),
        )
    };
    if written == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let length = target.iter().position(|value| *value == 0).unwrap_or(0);
    if length == 0 {
        return Ok(None);
    }
    Ok(Some((
        String::from_utf16(&target[..length])
            .map_err(|_| std::io::Error::other("QueryDosDevice returned invalid UTF-16"))?,
        drive,
    )))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn kernel_device_paths_resolve_beneath_the_selected_root() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("asset.bin");
        std::fs::write(&file, b"asset").unwrap();
        let filter = RootFilter::new(directory.path()).unwrap();
        let canonical = file.canonicalize().unwrap();
        let (_, drive) = filter.device_mapping.as_ref().unwrap();
        let canonical_text = canonical.to_string_lossy();
        let (_, suffix) = canonical_text.split_once(drive).unwrap();
        let device = &filter.device_mapping.as_ref().unwrap().0;
        let kernel_path = PathBuf::from(format!("{device}{suffix}"));
        assert_eq!(
            filter.include(&kernel_path).unwrap(),
            PathBuf::from("asset.bin")
        );
    }
}
