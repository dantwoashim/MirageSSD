use mirage_types::{GenerationId, MirageError, RepositoryId};

use crate::checked_slice::{array_at, read_u32, read_u64, write_bytes, write_u32, write_u64};
use crate::format::{
    FORMAT_VERSION, HEADER_ALIGNMENT, HEADER_SIZE, INDEX_HASH_LENGTH, INDEX_HASH_OFFSET, MAGIC,
    SECTION_COUNT, SECTION_DIRECTORY_OFFSET, SECTION_ENTRY_SIZE, Section, SectionKind, format_hash,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub repository_id: RepositoryId,
    pub generation_id: GenerationId,
    pub page_size: u64,
    pub index_hash: [u8; 32],
    pub file_length: u64,
    sections: [Section; SECTION_COUNT],
}

impl Header {
    pub fn new(
        repository_id: RepositoryId,
        generation_id: GenerationId,
        page_size: u64,
        index_hash: [u8; 32],
        file_length: u64,
        sections: [Section; SECTION_COUNT],
    ) -> Result<Self, MirageError> {
        let header = Self {
            repository_id,
            generation_id,
            page_size,
            index_hash,
            file_length,
            sections,
        };
        header.validate_sections()?;
        Ok(header)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, MirageError> {
        if bytes.len() < HEADER_SIZE {
            return Err(MirageError::manifest_invalid(
                "mount index header is truncated",
            ));
        }
        if &bytes[..MAGIC.len()] != MAGIC {
            return Err(MirageError::unsupported_layout(
                "mount index magic is invalid",
            ));
        }
        if read_u32(bytes, 8)? != FORMAT_VERSION {
            return Err(MirageError::unsupported_layout(
                "mount index version is unsupported",
            ));
        }
        if usize::try_from(read_u32(bytes, 12)?) != Ok(HEADER_SIZE) {
            return Err(MirageError::unsupported_layout(
                "mount index header size is unsupported",
            ));
        }
        if read_u32(bytes, 48)? != u32::try_from(SECTION_COUNT).unwrap_or(u32::MAX) {
            return Err(MirageError::manifest_invalid(
                "mount index section count is invalid",
            ));
        }
        if read_u32(bytes, 52)? != 0 {
            return Err(MirageError::unsupported_layout(
                "mount index flags are unsupported",
            ));
        }
        if array_at::<32>(bytes, 56)? != format_hash() {
            return Err(MirageError::unsupported_layout(
                "mount index format hash is unknown",
            ));
        }
        let file_length = read_u64(bytes, 120)?;
        if file_length != u64::try_from(bytes.len()).unwrap_or(u64::MAX) {
            return Err(MirageError::manifest_invalid(
                "mount index file length is contradictory",
            ));
        }

        let mut parsed: [Option<Section>; SECTION_COUNT] = [None; SECTION_COUNT];
        for entry_index in 0..SECTION_COUNT {
            let base = SECTION_DIRECTORY_OFFSET + entry_index * SECTION_ENTRY_SIZE;
            let kind = SectionKind::from_u32(read_u32(bytes, base)?)?;
            if kind != SectionKind::ALL[entry_index] {
                return Err(MirageError::manifest_invalid(
                    "mount index section directory is not canonical",
                ));
            }
            if read_u32(bytes, base + 4)? != kind.record_size() {
                return Err(MirageError::unsupported_layout(
                    "mount index record width is unknown",
                ));
            }
            let section = Section {
                kind,
                offset: read_u64(bytes, base + 8)?,
                length: read_u64(bytes, base + 16)?,
                count: read_u64(bytes, base + 24)?,
            };
            if parsed[kind.index()].replace(section).is_some() {
                return Err(MirageError::manifest_invalid(
                    "mount index section kind is duplicated",
                ));
            }
        }
        let sections: [Section; SECTION_COUNT] = parsed
            .into_iter()
            .map(|section| {
                section
                    .ok_or_else(|| MirageError::manifest_invalid("mount index section is missing"))
            })
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| MirageError::internal_invariant("section array width changed"))?;
        let header = Self {
            repository_id: RepositoryId::from_bytes(array_at(bytes, 16)?),
            generation_id: GenerationId::from_u64(read_u64(bytes, 32)?),
            page_size: read_u64(bytes, 40)?,
            index_hash: array_at(bytes, INDEX_HASH_OFFSET)?,
            file_length,
            sections,
        };
        header.validate_sections()?;
        Ok(header)
    }

    pub fn encode(&self) -> Result<[u8; HEADER_SIZE], MirageError> {
        self.validate_sections()?;
        let mut output = [0_u8; HEADER_SIZE];
        write_bytes(&mut output, 0, MAGIC)?;
        write_u32(&mut output, 8, FORMAT_VERSION)?;
        write_u32(
            &mut output,
            12,
            u32::try_from(HEADER_SIZE).unwrap_or(u32::MAX),
        )?;
        write_bytes(&mut output, 16, self.repository_id.as_bytes())?;
        write_u64(&mut output, 32, self.generation_id.as_u64())?;
        write_u64(&mut output, 40, self.page_size)?;
        write_u32(
            &mut output,
            48,
            u32::try_from(SECTION_COUNT).unwrap_or(u32::MAX),
        )?;
        write_u32(&mut output, 52, 0)?;
        write_bytes(&mut output, 56, &format_hash())?;
        write_bytes(&mut output, INDEX_HASH_OFFSET, &self.index_hash)?;
        write_u64(&mut output, 120, self.file_length)?;
        for (entry_index, section) in self.sections.iter().enumerate() {
            let base = SECTION_DIRECTORY_OFFSET + entry_index * SECTION_ENTRY_SIZE;
            write_u32(&mut output, base, section.kind as u32)?;
            write_u32(&mut output, base + 4, section.kind.record_size())?;
            write_u64(&mut output, base + 8, section.offset)?;
            write_u64(&mut output, base + 16, section.length)?;
            write_u64(&mut output, base + 24, section.count)?;
        }
        Ok(output)
    }

    #[must_use]
    pub const fn section(&self, kind: SectionKind) -> Section {
        self.sections[kind.index()]
    }

    pub fn compute_index_hash(bytes: &[u8]) -> Result<[u8; 32], MirageError> {
        if bytes.len() < INDEX_HASH_OFFSET + INDEX_HASH_LENGTH {
            return Err(MirageError::manifest_invalid(
                "mount index hash field is truncated",
            ));
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(&bytes[..INDEX_HASH_OFFSET]);
        hasher.update(&[0_u8; INDEX_HASH_LENGTH]);
        hasher.update(&bytes[INDEX_HASH_OFFSET + INDEX_HASH_LENGTH..]);
        Ok(*hasher.finalize().as_bytes())
    }

    fn validate_sections(&self) -> Result<(), MirageError> {
        let minimum = u64::try_from(HEADER_SIZE).unwrap_or(u64::MAX);
        let mut spans = Vec::with_capacity(SECTION_COUNT);
        for expected in SectionKind::ALL {
            let section = self.sections[expected.index()];
            if section.kind != expected {
                return Err(MirageError::manifest_invalid(
                    "mount index section order is invalid",
                ));
            }
            if section.offset < minimum || !section.offset.is_multiple_of(HEADER_ALIGNMENT) {
                return Err(MirageError::manifest_invalid(
                    "mount index section alignment is invalid",
                ));
            }
            let expected_length = section
                .count
                .checked_mul(u64::from(section.kind.record_size()))
                .ok_or_else(|| {
                    MirageError::manifest_invalid("mount index section size overflows")
                })?;
            if section.length != expected_length {
                return Err(MirageError::manifest_invalid(
                    "mount index section length is contradictory",
                ));
            }
            let end = section.end()?;
            if end > self.file_length {
                return Err(MirageError::manifest_invalid(
                    "mount index section exceeds file length",
                ));
            }
            if section.length != 0 {
                spans.push((section.offset, end));
            }
        }
        spans.sort_unstable();
        for pair in spans.windows(2) {
            if pair[0].1 > pair[1].0 {
                return Err(MirageError::manifest_invalid(
                    "mount index sections overlap",
                ));
            }
        }
        Ok(())
    }
}
