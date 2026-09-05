use std::convert::Infallible;

use minicbor::data::Type;
use minicbor::{Decoder, Encoder};
use mirage_backend::{ObjectKind, RemoteObjectRef};
use mirage_types::{
    CommitHash, ContentHash, DeviceId, ManifestHash, MirageError, RepositoryId, UpdateId,
};
use serde::{Deserialize, Serialize};

use crate::canonical::{FieldSet, decode_error, definite_map};
use crate::codec::{decode_object_ref, encode_object_ref};
use crate::signature::{CommitSigner, CommitVerifier, SignatureAlgorithm, SignatureEnvelope};

pub const COMMIT_FORMAT_VERSION: u32 = 1;
const MAX_COMMIT_BYTES: usize = 64 * 1024;
type EncodeError = minicbor::encode::Error<Infallible>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnsignedCommitBody {
    pub format_version: u32,
    pub repository_id: RepositoryId,
    pub sequence: u64,
    pub parent_commit: Option<CommitHash>,
    pub manifest_hash: ManifestHash,
    pub manifest_object: RemoteObjectRef,
    pub referenced_pack_set_hash: ContentHash,
    pub created_utc_ns: i128,
    pub writer_device_id: DeviceId,
    pub update_journal_id: Option<UpdateId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryCommit {
    pub body: UnsignedCommitBody,
    pub signature: SignatureEnvelope,
}

pub fn sign_commit(
    body: UnsignedCommitBody,
    signer: &dyn CommitSigner,
) -> Result<RepositoryCommit, MirageError> {
    validate_body(&body)?;
    let bytes = encode_unsigned_body(&body)?;
    let signature = SignatureEnvelope {
        algorithm: signer.algorithm(),
        key_id: signer.key_id(),
        signature: signer.sign(&bytes)?,
    };
    signature.validate_shape()?;
    Ok(RepositoryCommit { body, signature })
}

pub fn validate_commit(
    commit: &RepositoryCommit,
    verifier: &dyn CommitVerifier,
) -> Result<(), MirageError> {
    validate_body(&commit.body)?;
    commit.signature.validate_shape()?;
    verifier.verify(&encode_unsigned_body(&commit.body)?, &commit.signature)
}

fn validate_body(body: &UnsignedCommitBody) -> Result<(), MirageError> {
    if body.format_version != COMMIT_FORMAT_VERSION {
        return Err(MirageError::unsupported_layout(
            "commit format version is not supported",
        ));
    }
    if body.sequence == 0 && body.parent_commit.is_some() {
        return Err(MirageError::manifest_invalid(
            "trust-root commit cannot declare a parent",
        ));
    }
    if body.sequence != 0 && body.parent_commit.is_none() {
        return Err(MirageError::manifest_invalid(
            "non-root commit must declare a parent hash",
        ));
    }
    body.manifest_object.validate()?;
    if body.manifest_object.kind != ObjectKind::Manifest {
        return Err(MirageError::manifest_invalid(
            "commit manifest object has the wrong immutable object kind",
        ));
    }
    if body.manifest_object.content_hash.as_bytes() != body.manifest_hash.as_bytes() {
        return Err(MirageError::integrity_mismatch(
            "commit manifest hash and manifest object hash differ",
        ));
    }
    Ok(())
}

pub fn encode_commit(commit: &RepositoryCommit) -> Result<Vec<u8>, MirageError> {
    validate_body(&commit.body)?;
    commit.signature.validate_shape()?;
    let mut encoder = Encoder::new(Vec::new());
    encoder
        .map(2)
        .and_then(|encoder| encoder.u8(1))
        .map_err(encode_failure)?;
    encode_unsigned_body_into(&mut encoder, &commit.body).map_err(encode_failure)?;
    encoder.u8(2).map_err(encode_failure)?;
    encode_signature_into(&mut encoder, &commit.signature).map_err(encode_failure)?;
    Ok(encoder.into_writer())
}

pub fn commit_hash(commit: &RepositoryCommit) -> Result<CommitHash, MirageError> {
    let bytes = encode_commit(commit)?;
    Ok(CommitHash::from_bytes(*blake3::hash(&bytes).as_bytes()))
}

fn encode_unsigned_body(body: &UnsignedCommitBody) -> Result<Vec<u8>, MirageError> {
    let mut encoder = Encoder::new(Vec::new());
    encode_unsigned_body_into(&mut encoder, body).map_err(encode_failure)?;
    Ok(encoder.into_writer())
}

fn encode_unsigned_body_into(
    encoder: &mut Encoder<Vec<u8>>,
    body: &UnsignedCommitBody,
) -> Result<(), EncodeError> {
    encoder
        .map(10)?
        .u8(1)?
        .u32(body.format_version)?
        .u8(2)?
        .bytes(body.repository_id.as_bytes())?
        .u8(3)?
        .u64(body.sequence)?
        .u8(4)?;
    if let Some(parent) = body.parent_commit {
        encoder.bytes(parent.as_bytes())?;
    } else {
        encoder.null()?;
    }
    encoder.u8(5)?.bytes(body.manifest_hash.as_bytes())?.u8(6)?;
    encode_object_ref(encoder, &body.manifest_object)?;
    encoder
        .u8(7)?
        .bytes(body.referenced_pack_set_hash.as_bytes())?
        .u8(8)?
        .i128(body.created_utc_ns)?
        .u8(9)?
        .bytes(body.writer_device_id.as_bytes())?
        .u8(10)?;
    if let Some(update) = body.update_journal_id {
        encoder.bytes(update.as_bytes())?;
    } else {
        encoder.null()?;
    }
    Ok(())
}

fn encode_signature_into(
    encoder: &mut Encoder<Vec<u8>>,
    signature: &SignatureEnvelope,
) -> Result<(), EncodeError> {
    encoder
        .map(3)?
        .u8(1)?
        .u16(signature.algorithm.code())?
        .u8(2)?
        .bytes(&signature.key_id)?
        .u8(3)?
        .bytes(&signature.signature)?;
    Ok(())
}

fn encode_failure(error: EncodeError) -> MirageError {
    MirageError::internal_invariant("canonical commit encoding failed").with_source(error)
}

pub fn decode_commit_bounded(bytes: &[u8]) -> Result<RepositoryCommit, MirageError> {
    if bytes.len() > MAX_COMMIT_BYTES {
        return Err(MirageError::manifest_invalid(
            "commit exceeds the 64 KiB decode bound",
        ));
    }
    let mut decoder = Decoder::new(bytes);
    definite_map(&mut decoder, 2)?;
    let mut fields = FieldSet::default();
    let mut body = None;
    let mut signature = None;
    for _ in 0..2 {
        let key = decoder.u32().map_err(decode_error)?;
        fields.insert(key, 2)?;
        match key {
            1 => body = Some(decode_unsigned_body(&mut decoder)?),
            2 => signature = Some(decode_signature(&mut decoder)?),
            _ => unreachable!(),
        }
    }
    fields.require_all(2)?;
    if decoder.position() != bytes.len() {
        return Err(MirageError::manifest_invalid(
            "commit contains trailing CBOR data",
        ));
    }
    let commit = RepositoryCommit {
        body: body.ok_or_else(|| MirageError::manifest_invalid("commit body is missing"))?,
        signature: signature
            .ok_or_else(|| MirageError::manifest_invalid("commit signature is missing"))?,
    };
    validate_body(&commit.body)?;
    commit.signature.validate_shape()?;
    Ok(commit)
}

fn decode_unsigned_body(decoder: &mut Decoder<'_>) -> Result<UnsignedCommitBody, MirageError> {
    definite_map(decoder, 10)?;
    let mut fields = FieldSet::default();
    let mut format_version = None;
    let mut repository_id = None;
    let mut sequence = None;
    let mut parent_commit = None;
    let mut parent_seen = false;
    let mut manifest_hash = None;
    let mut manifest_object = None;
    let mut pack_set_hash = None;
    let mut created = None;
    let mut writer = None;
    let mut update = None;
    let mut update_seen = false;
    for _ in 0..10 {
        let key = decoder.u32().map_err(decode_error)?;
        fields.insert(key, 10)?;
        match key {
            1 => format_version = Some(decoder.u32().map_err(decode_error)?),
            2 => repository_id = Some(RepositoryId::from_bytes(decode_fixed::<16>(decoder)?)),
            3 => sequence = Some(decoder.u64().map_err(decode_error)?),
            4 => {
                parent_commit = decode_optional_hash(decoder)?;
                parent_seen = true;
            }
            5 => manifest_hash = Some(ManifestHash::from_bytes(decode_fixed::<32>(decoder)?)),
            6 => manifest_object = Some(decode_object_ref(decoder)?),
            7 => pack_set_hash = Some(ContentHash::from_bytes(decode_fixed::<32>(decoder)?)),
            8 => created = Some(decoder.i128().map_err(decode_error)?),
            9 => writer = Some(DeviceId::from_bytes(decode_fixed::<16>(decoder)?)),
            10 => {
                update = decode_optional_update(decoder)?;
                update_seen = true;
            }
            _ => unreachable!(),
        }
    }
    fields.require_all(10)?;
    if !parent_seen || !update_seen {
        return Err(MirageError::manifest_invalid(
            "commit optional field is missing",
        ));
    }
    Ok(UnsignedCommitBody {
        format_version: required(format_version)?,
        repository_id: required(repository_id)?,
        sequence: required(sequence)?,
        parent_commit,
        manifest_hash: required(manifest_hash)?,
        manifest_object: required(manifest_object)?,
        referenced_pack_set_hash: required(pack_set_hash)?,
        created_utc_ns: required(created)?,
        writer_device_id: required(writer)?,
        update_journal_id: update,
    })
}

fn decode_signature(decoder: &mut Decoder<'_>) -> Result<SignatureEnvelope, MirageError> {
    definite_map(decoder, 3)?;
    let mut fields = FieldSet::default();
    let mut algorithm = None;
    let mut key_id = None;
    let mut signature = None;
    for _ in 0..3 {
        let key = decoder.u32().map_err(decode_error)?;
        fields.insert(key, 3)?;
        match key {
            1 => {
                algorithm = Some(SignatureAlgorithm::from_code(
                    decoder.u16().map_err(decode_error)?,
                )?)
            }
            2 => key_id = Some(decode_fixed::<16>(decoder)?),
            3 => signature = Some(decoder.bytes().map_err(decode_error)?.to_vec()),
            _ => unreachable!(),
        }
    }
    fields.require_all(3)?;
    Ok(SignatureEnvelope {
        algorithm: required(algorithm)?,
        key_id: required(key_id)?,
        signature: required(signature)?,
    })
}

fn decode_optional_hash(decoder: &mut Decoder<'_>) -> Result<Option<CommitHash>, MirageError> {
    if decoder.datatype().map_err(decode_error)? == Type::Null {
        decoder.null().map_err(decode_error)?;
        Ok(None)
    } else {
        Ok(Some(CommitHash::from_bytes(decode_fixed::<32>(decoder)?)))
    }
}

fn decode_optional_update(decoder: &mut Decoder<'_>) -> Result<Option<UpdateId>, MirageError> {
    if decoder.datatype().map_err(decode_error)? == Type::Null {
        decoder.null().map_err(decode_error)?;
        Ok(None)
    } else {
        Ok(Some(UpdateId::from_bytes(decode_fixed::<16>(decoder)?)))
    }
}

fn decode_fixed<const N: usize>(decoder: &mut Decoder<'_>) -> Result<[u8; N], MirageError> {
    decoder
        .bytes()
        .map_err(decode_error)?
        .try_into()
        .map_err(|_| MirageError::manifest_invalid("commit byte string has an invalid length"))
}

fn required<T>(value: Option<T>) -> Result<T, MirageError> {
    value.ok_or_else(|| MirageError::manifest_invalid("commit map is missing a required field"))
}
