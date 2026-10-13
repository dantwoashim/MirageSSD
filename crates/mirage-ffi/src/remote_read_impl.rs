use super::*;

struct FrameLocation {
    key: FrameKey,
    aad: EncryptedFrameAad,
    range: mirage_types::CheckedRange,
    reference: mirage_backend::RemoteObjectRef,
    speculative: bool,
}

impl RemotePayloadStore {
    fn location(
        &self,
        record: &mirage_db::payload_remote::PayloadRemoteObject,
        index: u64,
    ) -> Result<FrameLocation, FetchFailure> {
        let total = u64::try_from(record.plaintext_length).map_err(|_| FetchFailure::Integrity)?;
        let (offset, length) = payload_frame_range(total, index).ok_or(FetchFailure::Integrity)?;
        let member = u64::try_from(record.member_offset).map_err(|_| FetchFailure::Integrity)?;
        let start = member.checked_add(offset).ok_or(FetchFailure::Integrity)?;
        let range =
            mirage_types::CheckedRange::new(start, length).map_err(|_| FetchFailure::Integrity)?;
        let object_length =
            u64::try_from(record.object_length).map_err(|_| FetchFailure::Integrity)?;
        if range.end_exclusive() > object_length {
            return Err(FetchFailure::Integrity);
        }
        let frame_start = index
            .checked_mul(PAYLOAD_FRAME_BYTES)
            .ok_or(FetchFailure::Integrity)?;
        let plain_length = u32::try_from(
            total
                .checked_sub(frame_start)
                .ok_or(FetchFailure::Integrity)?
                .min(PAYLOAD_FRAME_BYTES),
        )
        .map_err(|_| FetchFailure::Integrity)?;
        let root = *record
            .frame_hashes
            .get(usize::try_from(index).map_err(|_| FetchFailure::Integrity)?)
            .ok_or(FetchFailure::Integrity)?;
        let aad = EncryptedFrameAad {
            repository: self.repository,
            pack_id: record.payload_id,
            frame_index: index,
            plaintext_hash: mirage_types::PageHash::from_bytes(root),
            plaintext_length: plain_length,
        };
        let reference = mirage_backend::RemoteObjectRef {
            backend_id: mirage_backend::BackendId::new("drive")
                .map_err(|_| FetchFailure::Backend)?,
            provider_object_id: mirage_backend::ProviderObjectId::new(
                record.provider_object_id.clone(),
            )
            .map_err(|_| FetchFailure::Backend)?,
            immutable_revision: record
                .immutable_revision
                .as_ref()
                .map(|r| mirage_backend::ImmutableRevision::new(r.clone()))
                .transpose()
                .map_err(|_| FetchFailure::Integrity)?,
            byte_length: mirage_types::ByteCount::from_u64(object_length),
            content_hash: ContentHash::from_bytes(record.object_hash),
            kind: ObjectKind::Payload,
        };
        Ok(FrameLocation {
            key: FrameKey {
                payload: record.payload_id,
                index,
                root,
                length: plain_length,
                representation: record.object_hash,
                member_offset: member,
            },
            aad,
            range,
            reference,
            speculative: false,
        })
    }

    fn full_frame(&self, location: &FrameLocation) -> Result<Arc<[u8]>, FetchFailure> {
        if let Some(hit) = self
            .frames
            .lock()
            .map_err(|_| FetchFailure::Backend)?
            .get(&location.key)
        {
            return Ok(hit);
        }
        share_flight(&self.flights, location.key, || {
            let _permit = self.fetch_gate.acquire(location.speculative)?;
            // A previous owner may have filled the cache while we waited.
            if let Some(hit) = self
                .frames
                .lock()
                .map_err(|_| FetchFailure::Backend)?
                .get(&location.key)
            {
                return Ok(hit);
            }
            let class = if location.speculative {
                mirage_backend::FetchClass::ReadAhead
            } else {
                mirage_backend::FetchClass::BlockingRead
            };
            let encoded =
                fetch_remote_range(&self.backend, &location.reference, location.range, class)?;
            if encoded.len() as u64 != location.range.len() {
                return Err(FetchFailure::Integrity);
            }
            let (plain, catalog) = learn_verified_frame(&self.key, &encoded, location.aad)
                .map_err(|_| FetchFailure::Integrity)?;
            let charge = catalog.memory_bytes();
            // A one-sector frame cannot benefit from a partial transfer.
            if plain.len() > VERIFIED_SECTOR_BYTES {
                self.proofs
                    .lock()
                    .map_err(|_| FetchFailure::Backend)?
                    .insert(location.key, Arc::new(catalog), charge);
            }
            let bytes: Arc<[u8]> = plain.into();
            self.frames
                .lock()
                .map_err(|_| FetchFailure::Backend)?
                .insert(location.key, Arc::clone(&bytes), bytes.len());
            Ok(bytes)
        })
    }

    fn sector(
        &self,
        location: &FrameLocation,
        catalog: &Arc<FrameProofCatalog>,
        start: usize,
    ) -> Result<Arc<[u8]>, FetchFailure> {
        let key = (location.key, start);
        if let Some(hit) = self
            .sectors
            .lock()
            .map_err(|_| FetchFailure::Backend)?
            .get(&key)
        {
            return Ok(hit);
        }
        share_flight(&self.sector_flights, key, || {
            let _permit = self.fetch_gate.acquire(location.speculative)?;
            if let Some(hit) = self
                .sectors
                .lock()
                .map_err(|_| FetchFailure::Backend)?
                .get(&key)
            {
                return Ok(hit);
            }
            let length = VERIFIED_SECTOR_BYTES.min(location.key.length as usize - start);
            let absolute = location
                .range
                .start()
                .checked_add(ENCRYPTED_FRAME_PREFIX_BYTES as u64)
                .and_then(|offset| offset.checked_add(start as u64))
                .ok_or(FetchFailure::Integrity)?;
            let range = mirage_types::CheckedRange::new(absolute, length as u64)
                .map_err(|_| FetchFailure::Integrity)?;
            let class = if location.speculative {
                mirage_backend::FetchClass::ReadAhead
            } else {
                mirage_backend::FetchClass::BlockingRead
            };
            let encoded = fetch_remote_part(&self.backend, &location.reference, range, class)?;
            let bytes: Arc<[u8]> =
                decode_verified_sector(&self.key, catalog, location.aad, start, &encoded)
                    .map_err(|_| FetchFailure::Integrity)?
                    .into();
            self.sectors
                .lock()
                .map_err(|_| FetchFailure::Backend)?
                .insert(key, Arc::clone(&bytes), bytes.len());
            Ok(bytes)
        })
    }

    fn piece(
        &self,
        location: &FrameLocation,
        start: usize,
        length: usize,
    ) -> Result<(usize, Arc<[u8]>), FetchFailure> {
        let full = self
            .frames
            .lock()
            .map_err(|_| FetchFailure::Backend)?
            .get(&location.key);
        let catalog = self
            .proofs
            .lock()
            .map_err(|_| FetchFailure::Backend)?
            .get(&location.key);
        // Dense demand keeps the original full-AEAD path and coalesced transfers.
        if let Some(full) = full {
            return Ok((0, full));
        }
        if length < location.key.length as usize {
            let first = start / VERIFIED_SECTOR_BYTES * VERIFIED_SECTOR_BYTES;
            let mut cache = self.sectors.lock().map_err(|_| FetchFailure::Backend)?;
            let hits: Option<Vec<_>> = (first..start + length)
                .step_by(VERIFIED_SECTOR_BYTES)
                .map(|sector_start| {
                    cache
                        .get(&(location.key, sector_start))
                        .map(|bytes| (sector_start, bytes))
                })
                .collect();
            drop(cache);
            if let Some(hits) = hits {
                let mut bytes = Vec::with_capacity(length);
                for (sector_start, sector) in hits {
                    let lo = start.saturating_sub(sector_start);
                    let hi = (start + length - sector_start).min(sector.len());
                    bytes.extend_from_slice(&sector[lo..hi]);
                }
                return Ok((start, bytes.into()));
            }
        }
        if length == location.key.length as usize || catalog.is_none() {
            let full = self.full_frame(location)?;
            // Preserve only demanded sectors separately from the borrowed frame.
            if length < full.len() {
                let first = start / VERIFIED_SECTOR_BYTES * VERIFIED_SECTOR_BYTES;
                let end = start + length;
                let mut sectors = self.sectors.lock().map_err(|_| FetchFailure::Backend)?;
                for sector_start in (first..end).step_by(VERIFIED_SECTOR_BYTES) {
                    let sector_end = (sector_start + VERIFIED_SECTOR_BYTES).min(full.len());
                    let bytes: Arc<[u8]> = Arc::from(&full[sector_start..sector_end]);
                    sectors.insert(
                        (location.key, sector_start),
                        Arc::clone(&bytes),
                        bytes.len(),
                    );
                }
            }
            return Ok((0, full));
        }
        let catalog = catalog.ok_or(FetchFailure::Integrity)?;
        let end = start + length;
        let first = start / VERIFIED_SECTOR_BYTES * VERIFIED_SECTOR_BYTES;
        let mut bytes = Vec::with_capacity(length);
        for sector_start in (first..end).step_by(VERIFIED_SECTOR_BYTES) {
            let sector = self.sector(location, &catalog, sector_start)?;
            let lo = start.saturating_sub(sector_start);
            let hi = (end - sector_start).min(sector.len());
            bytes.extend_from_slice(&sector[lo..hi]);
        }
        Ok((start, bytes.into()))
    }

    pub(super) fn read_inner(
        &self,
        db: &Database,
        volume: RepositoryId,
        payload_id: &[u8; 16],
        offset: u64,
        length: usize,
        cancel: Option<&AtomicBool>,
    ) -> Result<Option<Vec<u8>>, MirageError> {
        if volume != self.repository {
            return Err(MirageError::integrity_mismatch(
                "payload belongs to another repository",
            ));
        }
        if !self.installed.load(Ordering::Acquire) {
            return Err(MirageError::backend_unavailable(
                "payload fetch requires an installed backend credential",
            ));
        }
        let Some(record) = self.publication(db, volume, payload_id)? else {
            return Ok(None);
        };
        let total =
            u64::try_from(record.plaintext_length).map_err(|_| failure(FetchFailure::Integrity))?;
        let end = offset
            .checked_add(length as u64)
            .ok_or_else(|| MirageError::invalid_argument("payload read range overflows"))?;
        if end > total {
            return Err(failure(FetchFailure::Integrity));
        }
        if length == 0 {
            return Ok(Some(Vec::new()));
        }
        let first = offset / PAYLOAD_FRAME_BYTES;
        let last = (end - 1) / PAYLOAD_FRAME_BYTES;
        let mut pending = first..=last;
        let mut output = Vec::with_capacity(length);
        loop {
            if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                return Err(MirageError::backend_unavailable("payload fetch cancelled"));
            }
            let batch: Vec<_> = pending
                .by_ref()
                .take(FRAME_FETCH_PARALLELISM)
                .map(|index| {
                    let mut location = self.location(&record, index)?;
                    location.speculative = cancel.is_some();
                    let frame_start = index * PAYLOAD_FRAME_BYTES;
                    let start = offset.saturating_sub(frame_start) as usize;
                    let take_end = (end - frame_start).min(u64::from(location.key.length)) as usize;
                    Ok((location, start, take_end - start))
                })
                .collect::<Result<_, FetchFailure>>()
                .map_err(failure)?;
            if batch.is_empty() {
                break;
            }
            // Single-frame reads (including sector hits) run inline. Multi-frame
            // hits also avoid spawning workers or copying whole cached frames.
            let results = if batch.len() == 1 {
                let (location, start, len) = &batch[0];
                vec![self.piece(location, *start, *len)]
            } else {
                std::thread::scope(|scope| {
                    let mut results = Vec::new();
                    for (location, start, len) in &batch {
                        let hit = self
                            .frames
                            .lock()
                            .ok()
                            .and_then(|mut cache| cache.get(&location.key));
                        results.push(match hit {
                            Some(bytes) => Ok((0, bytes)),
                            None => Err(scope.spawn(move || self.piece(location, *start, *len))),
                        });
                    }
                    results
                        .into_iter()
                        .map(|result| match result {
                            Ok(bytes) => Ok(bytes),
                            Err(worker) => worker.join().unwrap_or(Err(FetchFailure::Backend)),
                        })
                        .collect()
                })
            };
            for ((_, start, len), result) in batch.iter().zip(results) {
                let (base, bytes) = result.map_err(failure)?;
                let lo = start
                    .checked_sub(base)
                    .ok_or_else(|| failure(FetchFailure::Integrity))?;
                let hi = lo
                    .checked_add(*len)
                    .ok_or_else(|| failure(FetchFailure::Integrity))?;
                output.extend_from_slice(
                    bytes
                        .get(lo..hi)
                        .ok_or_else(|| failure(FetchFailure::Integrity))?,
                );
            }
        }
        if output.len() != length {
            return Err(failure(FetchFailure::Integrity));
        }
        Ok(Some(output))
    }
}

fn failure(error: FetchFailure) -> MirageError {
    match error {
        FetchFailure::Backend => MirageError::backend_unavailable("evicted payload fetch failed"),
        FetchFailure::Integrity => {
            MirageError::integrity_mismatch("evicted payload failed verification")
        }
    }
}

/// One immutable result per owner. A panicking owner still completes waiters
/// and releases its flight; failures do not poison the cache or persist.
fn share_flight<K: Copy + Ord>(
    flights: &Mutex<std::collections::BTreeMap<K, Arc<FrameFlight>>>,
    key: K,
    fetch: impl FnOnce() -> Result<Arc<[u8]>, FetchFailure>,
) -> Result<Arc<[u8]>, FetchFailure> {
    let (flight, owner) = {
        let mut flights = flights.lock().map_err(|_| FetchFailure::Backend)?;
        match flights.get(&key) {
            Some(flight) => (Arc::clone(flight), false),
            None => {
                let flight = Arc::new(FrameFlight {
                    done: Mutex::new(None),
                    ready: Condvar::new(),
                });
                flights.insert(key, Arc::clone(&flight));
                (flight, true)
            }
        }
    };
    if owner {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(fetch))
            .unwrap_or(Err(FetchFailure::Backend));
        *flight
            .done
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(result.clone());
        flight.ready.notify_all();
        if let Ok(mut flights) = flights.lock() {
            flights.remove(&key);
        }
        return result;
    }
    let mut done = flight.done.lock().map_err(|_| FetchFailure::Backend)?;
    while done.is_none() {
        done = flight.ready.wait(done).map_err(|_| FetchFailure::Backend)?;
    }
    done.clone().ok_or(FetchFailure::Backend)?
}
