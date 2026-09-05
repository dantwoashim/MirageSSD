use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use mirage_manifest::{RepositoryManifest, validate_manifest};
use mirage_types::{MirageError, MirageErrorKind};

use crate::checked_slice::write_bytes;
use crate::format::{HEADER_ALIGNMENT, HEADER_SIZE, SECTION_COUNT, Section, SectionKind, align_up};
use crate::header::Header;
use crate::name::{compare_names, ordinal_key};
use crate::reader::MountIndex;
use crate::record::{
    DirectoryRecord, ExtentRecord, FileRecord, NO_PARENT, PageRecord, RemoteLocationRecord,
};
use crate::string_table::StringInterner;

pub fn compile_to_bytes(manifest: &RepositoryManifest) -> Result<Vec<u8>, MirageError> {
    validate_manifest(manifest)?;
    let mut interner = StringInterner::default();
    let DirectoryLayout {
        order: directory_order,
        ranges: directory_ranges,
        source_to_new: source_to_directory,
    } = directory_layout(manifest)?;
    let file_children = sorted_file_children(manifest);

    let mut directories = Vec::with_capacity(directory_order.len());
    let mut files = Vec::with_capacity(manifest.files.len());
    let mut extents = Vec::with_capacity(manifest.extents.len());
    let mut pages = Vec::with_capacity(manifest.pages.len());
    let mut locations = Vec::with_capacity(manifest.pages.len());

    for (new_index, &source_index) in directory_order.iter().enumerate() {
        let source = &manifest.directories[source_index];
        let (first_directory, directory_count) = directory_ranges[new_index];
        let children = &file_children[source_index];
        let first_file = to_u32(files.len(), "file index")?;
        for &source_file_index in children {
            let source_file = &manifest.files[source_file_index];
            let extent_start = to_u32(extents.len(), "extent index")?;
            let extent_source_start = usize::try_from(source_file.extent_start)
                .map_err(|_| MirageError::manifest_invalid("extent start overflows"))?;
            let extent_source_count = usize::try_from(source_file.extent_count)
                .map_err(|_| MirageError::manifest_invalid("extent count overflows"))?;
            let extent_source_end = extent_source_start
                .checked_add(extent_source_count)
                .ok_or_else(|| MirageError::manifest_invalid("extent source slice overflows"))?;
            for source_extent in manifest
                .extents
                .get(extent_source_start..extent_source_end)
                .ok_or_else(|| MirageError::manifest_invalid("extent source slice is invalid"))?
            {
                let page_start = to_u32(pages.len(), "page ordinal")?;
                let source_page_start = usize::try_from(source_extent.page_start)
                    .map_err(|_| MirageError::manifest_invalid("page start overflows"))?;
                let source_page_count = usize::try_from(source_extent.page_count)
                    .map_err(|_| MirageError::manifest_invalid("page count overflows"))?;
                let source_page_end = source_page_start
                    .checked_add(source_page_count)
                    .ok_or_else(|| MirageError::manifest_invalid("page source slice overflows"))?;
                let source_pages = manifest
                    .pages
                    .get(source_page_start..source_page_end)
                    .ok_or_else(|| MirageError::manifest_invalid("page source slice is invalid"))?;
                for source_page in source_pages {
                    let source_location = manifest
                        .remote_locations
                        .get(usize::try_from(source_page.remote_location).map_err(|_| {
                            MirageError::manifest_invalid("remote location index overflows")
                        })?)
                        .ok_or_else(|| {
                            MirageError::manifest_invalid("remote location index is invalid")
                        })?;
                    let remote_location = to_u32(locations.len(), "remote location index")?;
                    locations.push(RemoteLocationRecord {
                        backend_id: interner.intern(source_location.object.backend_id.as_str())?,
                        provider_object_id: interner
                            .intern(source_location.object.provider_object_id.as_str())?,
                        immutable_revision: source_location
                            .object
                            .immutable_revision
                            .as_ref()
                            .map(|revision| interner.intern(revision.as_str()))
                            .transpose()?,
                        object_length: source_location.object.byte_length.as_u64(),
                        object_hash: *source_location.object.content_hash.as_bytes(),
                        object_kind: source_location.object.kind,
                        codec: source_location.codec,
                        pack_offset: source_location.offset,
                        encoded_length: source_location.encoded_length.as_u64(),
                    });
                    pages.push(PageRecord {
                        plaintext_hash: source_page.plaintext_hash,
                        logical_length: source_page.logical_length,
                        remote_location,
                    });
                }
                extents.push(ExtentRecord {
                    logical_offset: source_extent.logical_offset,
                    logical_length: source_extent.logical_length.as_u64(),
                    page_start,
                    page_count: source_extent.page_count,
                    tail_length: source_pages.last().map_or(0, |page| page.logical_length),
                });
            }
            files.push(FileRecord {
                parent_directory: to_u32(new_index, "directory index")?,
                class: source_file.class,
                name: interner.intern(&source_file.name)?,
                key: interner.intern(&ordinal_key(&source_file.name))?,
                logical_size: source_file.logical_size.as_u64(),
                stable_id: source_file.stable_id,
                extent_start,
                extent_count: source_file.extent_count,
                canonical_index: to_u32(files.len(), "canonical file index")?,
            });
        }
        let parent = source.parent.map_or(Ok(NO_PARENT), |parent| {
            let parent = usize::try_from(parent)
                .map_err(|_| MirageError::manifest_invalid("directory parent overflows"))?;
            source_to_directory
                .get(parent)
                .copied()
                .filter(|index| *index != u32::MAX)
                .ok_or_else(|| MirageError::manifest_invalid("directory parent is not compiled"))
        })?;
        directories.push(DirectoryRecord {
            parent,
            name: interner.intern(&source.name)?,
            key: interner.intern(&ordinal_key(&source.name))?,
            first_directory,
            directory_count,
            first_file,
            file_count: to_u32(children.len(), "file child count")?,
            canonical_index: to_u32(new_index, "canonical directory index")?,
        });
    }

    let strings = interner.into_bytes();
    let payloads = [
        (SectionKind::Strings, strings, None),
        (
            SectionKind::Directories,
            encode_records(&directories, SectionKind::Directories, |record, bytes| {
                record.encode(bytes)
            })?,
            Some(directories.len()),
        ),
        (
            SectionKind::Files,
            encode_records(&files, SectionKind::Files, |record, bytes| {
                record.encode(bytes)
            })?,
            Some(files.len()),
        ),
        (
            SectionKind::Extents,
            encode_records(&extents, SectionKind::Extents, |record, bytes| {
                record.encode(bytes)
            })?,
            Some(extents.len()),
        ),
        (
            SectionKind::Pages,
            encode_records(&pages, SectionKind::Pages, |record, bytes| {
                record.encode(bytes)
            })?,
            Some(pages.len()),
        ),
        (
            SectionKind::RemoteLocations,
            encode_records(&locations, SectionKind::RemoteLocations, |record, bytes| {
                record.encode(bytes)
            })?,
            Some(locations.len()),
        ),
    ];

    let mut output = vec![0_u8; HEADER_SIZE];
    let mut sections = Vec::with_capacity(SECTION_COUNT);
    for (kind, payload, record_count) in payloads {
        let aligned = align_up(
            u64::try_from(output.len())
                .map_err(|_| MirageError::manifest_invalid("index size overflows u64"))?,
            HEADER_ALIGNMENT,
        )?;
        output.resize(
            usize::try_from(aligned)
                .map_err(|_| MirageError::manifest_invalid("index exceeds address space"))?,
            0,
        );
        let offset = aligned;
        output.extend_from_slice(&payload);
        let count = match record_count {
            Some(count) => u64::try_from(count)
                .map_err(|_| MirageError::manifest_invalid("record count overflows u64"))?,
            None => u64::try_from(payload.len())
                .map_err(|_| MirageError::manifest_invalid("string count overflows u64"))?,
        };
        sections.push(Section {
            kind,
            offset,
            length: u64::try_from(payload.len())
                .map_err(|_| MirageError::manifest_invalid("section length overflows u64"))?,
            count,
        });
    }
    let sections: [Section; SECTION_COUNT] = sections
        .try_into()
        .map_err(|_| MirageError::internal_invariant("section count changed"))?;
    let mut header = Header::new(
        manifest.repository_id,
        manifest.generation_id,
        manifest.page_size.as_u64(),
        [0; 32],
        u64::try_from(output.len())
            .map_err(|_| MirageError::manifest_invalid("index length overflows u64"))?,
        sections,
    )?;
    write_bytes(&mut output, 0, &header.encode()?)?;
    header.index_hash = Header::compute_index_hash(&output)?;
    write_bytes(&mut output, 0, &header.encode()?)?;
    Ok(output)
}

pub fn compile_to_path(
    manifest: &RepositoryManifest,
    destination: &Path,
) -> Result<(), MirageError> {
    if destination.exists() {
        return Err(MirageError::invalid_argument(
            "immutable mount index destination already exists",
        ));
    }
    let bytes = compile_to_bytes(manifest)?;
    let temporary = temporary_path(destination)?;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(MirageError::from)?;
        file.write_all(&bytes).map_err(MirageError::from)?;
        file.sync_all().map_err(MirageError::from)?;
        drop(file);
        let verified = MountIndex::open(&temporary)?;
        if verified.header().index_hash != Header::compute_index_hash(&bytes)? {
            return Err(MirageError::integrity_mismatch(
                "reopened mount index hash differs from compiled bytes",
            ));
        }
        std::fs::rename(&temporary, destination).map_err(MirageError::from)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

struct DirectoryLayout {
    order: Vec<usize>,
    ranges: Vec<(u32, u32)>,
    source_to_new: Vec<u32>,
}

fn directory_layout(manifest: &RepositoryManifest) -> Result<DirectoryLayout, MirageError> {
    let root = manifest
        .directories
        .iter()
        .position(|directory| directory.parent.is_none())
        .ok_or_else(|| MirageError::manifest_invalid("manifest has no root directory"))?;
    let mut children = vec![Vec::new(); manifest.directories.len()];
    for (index, directory) in manifest.directories.iter().enumerate() {
        if let Some(parent) = directory.parent {
            children[usize::try_from(parent)
                .map_err(|_| MirageError::manifest_invalid("directory parent overflows"))?]
            .push(index);
        }
    }
    for group in &mut children {
        group.sort_by(|left, right| {
            compare_names(
                &manifest.directories[*left].name,
                &manifest.directories[*right].name,
            )
        });
    }
    let mut order = vec![root];
    let mut ranges = vec![(0, 0)];
    let mut source_to_new = vec![u32::MAX; manifest.directories.len()];
    source_to_new[root] = 0;
    let mut cursor = 0;
    while cursor < order.len() {
        let first = to_u32(order.len(), "directory child index")?;
        let group = &children[order[cursor]];
        ranges[cursor] = (first, to_u32(group.len(), "directory child count")?);
        for &child in group {
            source_to_new[child] = to_u32(order.len(), "directory index")?;
            order.push(child);
            ranges.push((0, 0));
        }
        cursor += 1;
    }
    Ok(DirectoryLayout {
        order,
        ranges,
        source_to_new,
    })
}

fn sorted_file_children(manifest: &RepositoryManifest) -> Vec<Vec<usize>> {
    let mut children = vec![Vec::new(); manifest.directories.len()];
    for (index, file) in manifest.files.iter().enumerate() {
        children[file.parent_directory as usize].push(index);
    }
    for group in &mut children {
        group.sort_by(|left, right| {
            compare_names(&manifest.files[*left].name, &manifest.files[*right].name)
        });
    }
    children
}

fn encode_records<T>(
    records: &[T],
    kind: SectionKind,
    mut encode: impl FnMut(&T, &mut [u8]) -> Result<(), MirageError>,
) -> Result<Vec<u8>, MirageError> {
    let width = usize::try_from(kind.record_size())
        .map_err(|_| MirageError::internal_invariant("record width overflows usize"))?;
    let length = records
        .len()
        .checked_mul(width)
        .ok_or_else(|| MirageError::manifest_invalid("record section allocation overflows"))?;
    let mut output = vec![0_u8; length];
    for (index, record) in records.iter().enumerate() {
        let start = index * width;
        encode(record, &mut output[start..start + width])?;
    }
    Ok(output)
}

fn to_u32(value: usize, label: &str) -> Result<u32, MirageError> {
    u32::try_from(value).map_err(|_| MirageError::manifest_invalid(format!("{label} exceeds u32")))
}

fn temporary_path(destination: &Path) -> Result<PathBuf, MirageError> {
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| MirageError::invalid_argument("index file name must be valid UTF-8"))?;
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    for suffix in 0_u16..=u16::MAX {
        let candidate = parent.join(format!(".{name}.mirage-tmp-{suffix}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(MirageError::new(
        MirageErrorKind::Io,
        MirageErrorKind::Io.default_code(),
        "no temporary index name is available",
    ))
}
