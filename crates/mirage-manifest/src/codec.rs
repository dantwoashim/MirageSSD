use std::convert::Infallible;

use minicbor::data::Type;
use minicbor::{Decoder, Encoder};
use mirage_backend::{BackendId, ImmutableRevision, ObjectKind, ProviderObjectId, RemoteObjectRef};
use mirage_types::{
    ByteCount, ContentHash, GenerationId, MirageError, PageHash, RepositoryId, StableFileId,
};

use crate::canonical::{FieldSet, decode_error, definite_array, definite_map};
use crate::model::{
    Codec, DirectoryRecord, ExtentRecord, FileClass, FileRecord, PageRecord, RemoteLocation,
    RepositoryManifest,
};
use crate::validate::{
    MAX_DIRECTORIES, MAX_EXTENTS, MAX_FILES, MAX_PAGES, MAX_REMOTE_LOCATIONS, validate_manifest,
};

type EncodeError = minicbor::encode::Error<Infallible>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeLimits {
    pub max_input_bytes: usize,
    pub max_directories: usize,
    pub max_files: usize,
    pub max_extents: usize,
    pub max_pages: usize,
    pub max_remote_locations: usize,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 1024 * 1024 * 1024,
            max_directories: MAX_DIRECTORIES,
            max_files: MAX_FILES,
            max_extents: MAX_EXTENTS,
            max_pages: MAX_PAGES,
            max_remote_locations: MAX_REMOTE_LOCATIONS,
        }
    }
}

pub fn encode_manifest(manifest: &RepositoryManifest) -> Result<Vec<u8>, MirageError> {
    validate_manifest(manifest)?;
    encode_manifest_inner(manifest).map_err(|error| {
        MirageError::internal_invariant("canonical manifest encoding failed").with_source(error)
    })
}

fn encode_manifest_inner(manifest: &RepositoryManifest) -> Result<Vec<u8>, EncodeError> {
    let mut encoder = Encoder::new(Vec::new());
    encoder.map(9)?;
    encoder.u8(1)?.u32(manifest.format_version)?;
    encoder.u8(2)?.bytes(manifest.repository_id.as_bytes())?;
    encoder.u8(3)?.u64(manifest.generation_id.as_u64())?;
    encoder.u8(4)?.u64(manifest.page_size.as_u64())?;

    encoder.u8(5)?.array(manifest.directories.len() as u64)?;
    for directory in &manifest.directories {
        encode_directory(&mut encoder, directory)?;
    }
    encoder.u8(6)?.array(manifest.files.len() as u64)?;
    for file in &manifest.files {
        encode_file(&mut encoder, file)?;
    }
    encoder.u8(7)?.array(manifest.extents.len() as u64)?;
    for extent in &manifest.extents {
        encode_extent(&mut encoder, extent)?;
    }
    encoder.u8(8)?.array(manifest.pages.len() as u64)?;
    for page in &manifest.pages {
        encode_page(&mut encoder, page)?;
    }
    encoder
        .u8(9)?
        .array(manifest.remote_locations.len() as u64)?;
    for location in &manifest.remote_locations {
        encode_remote_location(&mut encoder, location)?;
    }
    Ok(encoder.into_writer())
}

fn encode_directory(
    encoder: &mut Encoder<Vec<u8>>,
    directory: &DirectoryRecord,
) -> Result<(), EncodeError> {
    encoder.map(2)?.u8(1)?;
    match directory.parent {
        Some(parent) => {
            encoder.u32(parent)?;
        }
        None => {
            encoder.null()?;
        }
    }
    encoder.u8(2)?.str(&directory.name)?;
    Ok(())
}

fn encode_file(encoder: &mut Encoder<Vec<u8>>, file: &FileRecord) -> Result<(), EncodeError> {
    encoder
        .map(7)?
        .u8(1)?
        .u32(file.parent_directory)?
        .u8(2)?
        .str(&file.name)?
        .u8(3)?
        .u64(file.logical_size.as_u64())?
        .u8(4)?
        .u64(file.stable_id.as_u64())?
        .u8(5)?
        .u8(file.class.code())?
        .u8(6)?
        .u32(file.extent_start)?
        .u8(7)?
        .u32(file.extent_count)?;
    Ok(())
}

fn encode_extent(encoder: &mut Encoder<Vec<u8>>, extent: &ExtentRecord) -> Result<(), EncodeError> {
    encoder
        .map(4)?
        .u8(1)?
        .u64(extent.logical_offset)?
        .u8(2)?
        .u64(extent.logical_length.as_u64())?
        .u8(3)?
        .u32(extent.page_start)?
        .u8(4)?
        .u32(extent.page_count)?;
    Ok(())
}

fn encode_page(encoder: &mut Encoder<Vec<u8>>, page: &PageRecord) -> Result<(), EncodeError> {
    encoder
        .map(3)?
        .u8(1)?
        .bytes(page.plaintext_hash.as_bytes())?
        .u8(2)?
        .u32(page.logical_length)?
        .u8(3)?
        .u32(page.remote_location)?;
    Ok(())
}

fn encode_remote_location(
    encoder: &mut Encoder<Vec<u8>>,
    location: &RemoteLocation,
) -> Result<(), EncodeError> {
    encoder
        .map(9)?
        .u8(1)?
        .str(location.object.backend_id.as_str())?
        .u8(2)?
        .str(location.object.provider_object_id.as_str())?
        .u8(3)?;
    encode_optional_revision(encoder, location.object.immutable_revision.as_ref())?;
    encoder
        .u8(4)?
        .u64(location.object.byte_length.as_u64())?
        .u8(5)?
        .bytes(location.object.content_hash.as_bytes())?
        .u8(6)?
        .u8(object_kind_code(location.object.kind))?
        .u8(7)?
        .u64(location.offset)?
        .u8(8)?
        .u64(location.encoded_length.as_u64())?
        .u8(9)?
        .u8(location.codec.code())?;
    Ok(())
}

pub(crate) fn encode_object_ref(
    encoder: &mut Encoder<Vec<u8>>,
    object: &RemoteObjectRef,
) -> Result<(), EncodeError> {
    encoder
        .map(6)?
        .u8(1)?
        .str(object.backend_id.as_str())?
        .u8(2)?
        .str(object.provider_object_id.as_str())?
        .u8(3)?;
    encode_optional_revision(encoder, object.immutable_revision.as_ref())?;
    encoder
        .u8(4)?
        .u64(object.byte_length.as_u64())?
        .u8(5)?
        .bytes(object.content_hash.as_bytes())?
        .u8(6)?
        .u8(object_kind_code(object.kind))?;
    Ok(())
}

fn encode_optional_revision(
    encoder: &mut Encoder<Vec<u8>>,
    revision: Option<&ImmutableRevision>,
) -> Result<(), EncodeError> {
    if let Some(revision) = revision {
        encoder.str(revision.as_str())?;
    } else {
        encoder.null()?;
    }
    Ok(())
}

pub fn decode_manifest_bounded(
    bytes: &[u8],
    limits: DecodeLimits,
) -> Result<RepositoryManifest, MirageError> {
    if bytes.len() > limits.max_input_bytes {
        return Err(MirageError::manifest_invalid(
            "manifest input exceeds the decode byte budget",
        ));
    }
    let mut decoder = Decoder::new(bytes);
    definite_map(&mut decoder, 9)?;
    let mut fields = FieldSet::default();
    let mut format_version = None;
    let mut repository_id = None;
    let mut generation_id = None;
    let mut page_size = None;
    let mut directories = None;
    let mut files = None;
    let mut extents = None;
    let mut pages = None;
    let mut remote_locations = None;

    for _ in 0..9 {
        let key = decoder.u32().map_err(decode_error)?;
        fields.insert(key, 9)?;
        match key {
            1 => format_version = Some(decoder.u32().map_err(decode_error)?),
            2 => repository_id = Some(RepositoryId::from_bytes(decode_fixed::<16>(&mut decoder)?)),
            3 => generation_id = Some(GenerationId::from_u64(decoder.u64().map_err(decode_error)?)),
            4 => page_size = Some(ByteCount::from_u64(decoder.u64().map_err(decode_error)?)),
            5 => directories = Some(decode_directories(&mut decoder, limits.max_directories)?),
            6 => files = Some(decode_files(&mut decoder, limits.max_files)?),
            7 => extents = Some(decode_extents(&mut decoder, limits.max_extents)?),
            8 => pages = Some(decode_pages(&mut decoder, limits.max_pages)?),
            9 => {
                remote_locations = Some(decode_remote_locations(
                    &mut decoder,
                    limits.max_remote_locations,
                )?)
            }
            _ => unreachable!("FieldSet rejects unknown keys"),
        }
    }
    fields.require_all(9)?;
    if decoder.position() != bytes.len() {
        return Err(MirageError::manifest_invalid(
            "manifest contains trailing CBOR data",
        ));
    }
    let manifest = RepositoryManifest {
        format_version: required(format_version)?,
        repository_id: required(repository_id)?,
        generation_id: required(generation_id)?,
        page_size: required(page_size)?,
        directories: required(directories)?,
        files: required(files)?,
        extents: required(extents)?,
        pages: required(pages)?,
        remote_locations: required(remote_locations)?,
    };
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn decode_directories(
    decoder: &mut Decoder<'_>,
    maximum: usize,
) -> Result<Vec<DirectoryRecord>, MirageError> {
    decode_array(decoder, maximum, "directory", |decoder| {
        definite_map(decoder, 2)?;
        let mut fields = FieldSet::default();
        let mut parent = None;
        let mut parent_seen = false;
        let mut name = None;
        for _ in 0..2 {
            let key = decoder.u32().map_err(decode_error)?;
            fields.insert(key, 2)?;
            match key {
                1 => {
                    parent = decode_optional_u32(decoder)?;
                    parent_seen = true;
                }
                2 => name = Some(decoder.str().map_err(decode_error)?.to_string()),
                _ => unreachable!(),
            }
        }
        fields.require_all(2)?;
        if !parent_seen {
            return Err(MirageError::manifest_invalid("directory parent is missing"));
        }
        Ok(DirectoryRecord {
            parent,
            name: required(name)?,
        })
    })
}

fn decode_files(decoder: &mut Decoder<'_>, maximum: usize) -> Result<Vec<FileRecord>, MirageError> {
    decode_array(decoder, maximum, "file", |decoder| {
        definite_map(decoder, 7)?;
        let mut fields = FieldSet::default();
        let mut parent = None;
        let mut name = None;
        let mut size = None;
        let mut stable_id = None;
        let mut class = None;
        let mut extent_start = None;
        let mut extent_count = None;
        for _ in 0..7 {
            let key = decoder.u32().map_err(decode_error)?;
            fields.insert(key, 7)?;
            match key {
                1 => parent = Some(decoder.u32().map_err(decode_error)?),
                2 => name = Some(decoder.str().map_err(decode_error)?.to_string()),
                3 => size = Some(ByteCount::from_u64(decoder.u64().map_err(decode_error)?)),
                4 => stable_id = Some(StableFileId::from_u64(decoder.u64().map_err(decode_error)?)),
                5 => class = Some(FileClass::from_code(decoder.u8().map_err(decode_error)?)?),
                6 => extent_start = Some(decoder.u32().map_err(decode_error)?),
                7 => extent_count = Some(decoder.u32().map_err(decode_error)?),
                _ => unreachable!(),
            }
        }
        fields.require_all(7)?;
        Ok(FileRecord {
            parent_directory: required(parent)?,
            name: required(name)?,
            logical_size: required(size)?,
            stable_id: required(stable_id)?,
            class: required(class)?,
            extent_start: required(extent_start)?,
            extent_count: required(extent_count)?,
        })
    })
}

fn decode_extents(
    decoder: &mut Decoder<'_>,
    maximum: usize,
) -> Result<Vec<ExtentRecord>, MirageError> {
    decode_array(decoder, maximum, "extent", |decoder| {
        definite_map(decoder, 4)?;
        let mut fields = FieldSet::default();
        let mut offset = None;
        let mut length = None;
        let mut page_start = None;
        let mut page_count = None;
        for _ in 0..4 {
            let key = decoder.u32().map_err(decode_error)?;
            fields.insert(key, 4)?;
            match key {
                1 => offset = Some(decoder.u64().map_err(decode_error)?),
                2 => length = Some(ByteCount::from_u64(decoder.u64().map_err(decode_error)?)),
                3 => page_start = Some(decoder.u32().map_err(decode_error)?),
                4 => page_count = Some(decoder.u32().map_err(decode_error)?),
                _ => unreachable!(),
            }
        }
        fields.require_all(4)?;
        Ok(ExtentRecord {
            logical_offset: required(offset)?,
            logical_length: required(length)?,
            page_start: required(page_start)?,
            page_count: required(page_count)?,
        })
    })
}

fn decode_pages(decoder: &mut Decoder<'_>, maximum: usize) -> Result<Vec<PageRecord>, MirageError> {
    decode_array(decoder, maximum, "page", |decoder| {
        definite_map(decoder, 3)?;
        let mut fields = FieldSet::default();
        let mut hash = None;
        let mut length = None;
        let mut remote = None;
        for _ in 0..3 {
            let key = decoder.u32().map_err(decode_error)?;
            fields.insert(key, 3)?;
            match key {
                1 => hash = Some(PageHash::from_bytes(decode_fixed::<32>(decoder)?)),
                2 => length = Some(decoder.u32().map_err(decode_error)?),
                3 => remote = Some(decoder.u32().map_err(decode_error)?),
                _ => unreachable!(),
            }
        }
        fields.require_all(3)?;
        Ok(PageRecord {
            plaintext_hash: required(hash)?,
            logical_length: required(length)?,
            remote_location: required(remote)?,
        })
    })
}

fn decode_remote_locations(
    decoder: &mut Decoder<'_>,
    maximum: usize,
) -> Result<Vec<RemoteLocation>, MirageError> {
    decode_array(decoder, maximum, "remote location", |decoder| {
        definite_map(decoder, 9)?;
        let mut fields = FieldSet::default();
        let mut backend = None;
        let mut provider = None;
        let mut revision = None;
        let mut revision_seen = false;
        let mut object_length = None;
        let mut object_hash = None;
        let mut kind = None;
        let mut offset = None;
        let mut encoded_length = None;
        let mut codec = None;
        for _ in 0..9 {
            let key = decoder.u32().map_err(decode_error)?;
            fields.insert(key, 9)?;
            match key {
                1 => backend = Some(BackendId::new(decoder.str().map_err(decode_error)?)?),
                2 => provider = Some(ProviderObjectId::new(decoder.str().map_err(decode_error)?)?),
                3 => {
                    revision = decode_optional_revision(decoder)?;
                    revision_seen = true;
                }
                4 => {
                    object_length = Some(ByteCount::from_u64(decoder.u64().map_err(decode_error)?))
                }
                5 => object_hash = Some(ContentHash::from_bytes(decode_fixed::<32>(decoder)?)),
                6 => kind = Some(decode_object_kind(decoder.u8().map_err(decode_error)?)?),
                7 => offset = Some(decoder.u64().map_err(decode_error)?),
                8 => {
                    encoded_length = Some(ByteCount::from_u64(decoder.u64().map_err(decode_error)?))
                }
                9 => codec = Some(Codec::from_code(decoder.u8().map_err(decode_error)?)?),
                _ => unreachable!(),
            }
        }
        fields.require_all(9)?;
        if !revision_seen {
            return Err(MirageError::manifest_invalid(
                "object revision field is missing",
            ));
        }
        Ok(RemoteLocation {
            object: RemoteObjectRef {
                backend_id: required(backend)?,
                provider_object_id: required(provider)?,
                immutable_revision: revision,
                byte_length: required(object_length)?,
                content_hash: required(object_hash)?,
                kind: required(kind)?,
            },
            offset: required(offset)?,
            encoded_length: required(encoded_length)?,
            codec: required(codec)?,
        })
    })
}

pub(crate) fn decode_object_ref(decoder: &mut Decoder<'_>) -> Result<RemoteObjectRef, MirageError> {
    definite_map(decoder, 6)?;
    let mut fields = FieldSet::default();
    let mut backend = None;
    let mut provider = None;
    let mut revision = None;
    let mut revision_seen = false;
    let mut length = None;
    let mut hash = None;
    let mut kind = None;
    for _ in 0..6 {
        let key = decoder.u32().map_err(decode_error)?;
        fields.insert(key, 6)?;
        match key {
            1 => backend = Some(BackendId::new(decoder.str().map_err(decode_error)?)?),
            2 => provider = Some(ProviderObjectId::new(decoder.str().map_err(decode_error)?)?),
            3 => {
                revision = decode_optional_revision(decoder)?;
                revision_seen = true;
            }
            4 => length = Some(ByteCount::from_u64(decoder.u64().map_err(decode_error)?)),
            5 => hash = Some(ContentHash::from_bytes(decode_fixed::<32>(decoder)?)),
            6 => kind = Some(decode_object_kind(decoder.u8().map_err(decode_error)?)?),
            _ => unreachable!(),
        }
    }
    fields.require_all(6)?;
    if !revision_seen {
        return Err(MirageError::manifest_invalid(
            "object revision field is missing",
        ));
    }
    Ok(RemoteObjectRef {
        backend_id: required(backend)?,
        provider_object_id: required(provider)?,
        immutable_revision: revision,
        byte_length: required(length)?,
        content_hash: required(hash)?,
        kind: required(kind)?,
    })
}

fn decode_array<T>(
    decoder: &mut Decoder<'_>,
    maximum: usize,
    label: &str,
    mut item: impl FnMut(&mut Decoder<'_>) -> Result<T, MirageError>,
) -> Result<Vec<T>, MirageError> {
    let count = definite_array(decoder, maximum, label)?;
    let mut output = Vec::new();
    output
        .try_reserve(count.min(4096))
        .map_err(|_| MirageError::manifest_invalid("manifest allocation budget exhausted"))?;
    for _ in 0..count {
        output.push(item(decoder)?);
    }
    Ok(output)
}

fn decode_fixed<const N: usize>(decoder: &mut Decoder<'_>) -> Result<[u8; N], MirageError> {
    let bytes = decoder.bytes().map_err(decode_error)?;
    bytes
        .try_into()
        .map_err(|_| MirageError::manifest_invalid("CBOR byte string has an invalid fixed length"))
}

fn decode_optional_u32(decoder: &mut Decoder<'_>) -> Result<Option<u32>, MirageError> {
    if decoder.datatype().map_err(decode_error)? == Type::Null {
        decoder.null().map_err(decode_error)?;
        Ok(None)
    } else {
        Ok(Some(decoder.u32().map_err(decode_error)?))
    }
}

fn decode_optional_revision(
    decoder: &mut Decoder<'_>,
) -> Result<Option<ImmutableRevision>, MirageError> {
    if decoder.datatype().map_err(decode_error)? == Type::Null {
        decoder.null().map_err(decode_error)?;
        Ok(None)
    } else {
        Ok(Some(ImmutableRevision::new(
            decoder.str().map_err(decode_error)?,
        )?))
    }
}

fn required<T>(value: Option<T>) -> Result<T, MirageError> {
    value.ok_or_else(|| MirageError::manifest_invalid("CBOR map is missing a required field"))
}

pub(crate) const fn object_kind_code(kind: ObjectKind) -> u8 {
    match kind {
        ObjectKind::RepositoryConfig => 0,
        ObjectKind::Pack => 1,
        ObjectKind::Manifest => 2,
        ObjectKind::Commit => 3,
        ObjectKind::Profile => 4,
    }
}

fn decode_object_kind(code: u8) -> Result<ObjectKind, MirageError> {
    match code {
        0 => Ok(ObjectKind::RepositoryConfig),
        1 => Ok(ObjectKind::Pack),
        2 => Ok(ObjectKind::Manifest),
        3 => Ok(ObjectKind::Commit),
        4 => Ok(ObjectKind::Profile),
        _ => Err(MirageError::manifest_invalid(
            "unknown immutable object kind",
        )),
    }
}
