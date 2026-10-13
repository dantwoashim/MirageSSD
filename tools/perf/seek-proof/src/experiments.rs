use super::*;

pub(super) fn run() -> serde_json::Value {
    // Public deterministic TEST key. No installed repository key is read.
    let key = RepositoryKey::from_bytes([0x41; 32]);
    let mut positive = 0;
    let mut negative = 0;
    let mut rng = 20261012_u64;
    for length in [
        0,
        1,
        63,
        64,
        65,
        4095,
        4096,
        4097,
        65535,
        65536,
        65537,
        FRAME - 1,
        FRAME,
    ] {
        let (plain, encoded, mut id) = fixture(length, length as u64, &key);
        for granule in [4096, 65536, 262144] {
            let envelope = make_index(&key, &encoded, id, granule).unwrap();
            id.trusted_sidecar_hash = *blake3::hash(&envelope).as_bytes();
            let index = VerifiedIndex::open(&key, &envelope, id).unwrap();
            let mut queries = vec![(0, 0), (length, 0), (0, length)];
            for boundary in [1, 63, 64, 4096, 65536, 262144] {
                if boundary < length {
                    queries.push((boundary - 1, 2));
                }
            }
            for _ in 0..64 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let start = rng as usize % (length + 1);
                let count = (rng.rotate_left(17) as usize % 100001).min(length - start);
                queries.push((start, count));
            }
            for (offset, count) in queries {
                let mut requests = 0;
                let output = index
                    .read(offset, count, |start, size| {
                        requests += 1;
                        assert!(start >= PREFIX && start + size <= PREFIX + length);
                        Ok(encoded[start..start + size].to_vec())
                    })
                    .unwrap();
                assert_eq!(output, plain[offset..offset + count]);
                assert_eq!(requests, usize::from(count != 0));
                positive += 1;
            }
            for (offset, count) in [(length, 1), (length + 1, 0), (usize::MAX, 2)] {
                assert!(
                    index
                        .read(offset, count, |_, _| panic!("invalid range fetched"))
                        .is_err()
                );
                negative += 1;
            }
        }
    }

    let (plain, encoded, mut id) = fixture(FRAME, 99, &key);
    let envelope = make_index(&key, &encoded, id, 65536).unwrap();
    id.trusted_sidecar_hash = *blake3::hash(&envelope).as_bytes();
    let index = VerifiedIndex::open(&key, &envelope, id).unwrap();
    let original_hash = *blake3::hash(&encoded).as_bytes();
    // Every byte of the envelope, including its nonce and tag, is protected.
    for position in 0..envelope.len() {
        let mut corrupt = envelope.clone();
        corrupt[position] ^= 1;
        assert!(VerifiedIndex::open(&key, &corrupt, id).is_err());
        negative += 1;
    }
    for length in (0..envelope.len()).step_by(13) {
        assert!(VerifiedIndex::open(&key, &envelope[..length], id).is_err());
        negative += 1;
    }
    for field in 0..7 {
        let mut wrong = id;
        match field {
            0 => wrong.aad.repository = RepositoryId::from_bytes([0x42; 16]),
            1 => wrong.aad.pack_id[0] ^= 1,
            2 => wrong.aad.frame_index += 1,
            3 => wrong.aad.plaintext_hash = PageHash::from_bytes([0; 32]),
            4 => wrong.aad.plaintext_length -= 1,
            5 => wrong.generation += 1,
            6 => wrong.ciphertext_hash[0] ^= 1,
            _ => unreachable!(),
        }
        assert!(VerifiedIndex::open(&key, &envelope, wrong).is_err());
        negative += 1;
    }
    assert!(VerifiedIndex::open(&RepositoryKey::from_bytes([0x42; 32]), &envelope, id).is_err());
    negative += 1;
    for position in 0..512 {
        assert!(
            index
                .read(0, 4096, |start, count| {
                    let mut bytes = encoded[start..start + count].to_vec();
                    bytes[position * 127 % count] ^= 0x80;
                    Ok(bytes)
                })
                .is_err()
        );
        negative += 1;
    }
    for delta in [-1_i64, 1] {
        assert!(
            index
                .read(65536, 4096, |start, count| {
                    let wrong_start = (start as i64 + delta) as usize;
                    Ok(encoded[wrong_start..wrong_start + count].to_vec())
                })
                .is_err()
        );
        negative += 1;
    }
    for extra in [false, true] {
        assert!(
            index
                .read(0, 4096, |start, count| {
                    let size = if extra { count + 1 } else { count - 1 };
                    Ok(encoded[start..start + size].to_vec())
                })
                .is_err()
        );
        negative += 1;
    }
    let mut bad_original = encoded.clone();
    bad_original[PREFIX + 100] ^= 1;
    assert!(make_index(&key, &bad_original, id, 65536).is_err());
    negative += 1;
    let (_, other_encoded, other_id) = fixture(FRAME, 100, &key);
    assert!(VerifiedIndex::open(&key, &envelope, other_id).is_err());
    assert!(
        index
            .read(0, 4096, |start, count| Ok(other_encoded
                [start..start + count]
                .to_vec()))
            .is_err()
    );
    negative += 2;

    // A range proof authenticates requested content, not unread bytes or the old
    // whole-frame tag. This accepted case is deliberate and explicitly reported.
    let mut unrelated_damage = encoded.clone();
    unrelated_damage[PREFIX + FRAME - 1] ^= 1;
    *unrelated_damage.last_mut().unwrap() ^= 1;
    let selected = index
        .read(0, 4096, |start, count| {
            Ok(unrelated_damage[start..start + count].to_vec())
        })
        .unwrap();
    assert_eq!(selected, plain[..4096]);
    assert!(decode_encrypted_frame(&key, &unrelated_damage, id.aad).is_err());
    positive += 1;

    let mut samples = Vec::new();
    for round in 0..9 {
        for position in 0..3 {
            let variant = (round + position) % 3;
            let mut times = Vec::new();
            let mut transferred = 0;
            let mut checksum = 0_u64;
            for iteration in 0..100 {
                let offset = ((iteration * 7919) % (FRAME / 4096)) * 4096;
                let start = Instant::now();
                let result = match variant {
                    0 => {
                        transferred += encoded.len();
                        let response = black_box(encoded.clone());
                        let full = decode_encrypted_frame(&key, &response, id.aad).unwrap();
                        full[offset..offset + 4096].to_vec()
                    }
                    1 => index
                        .read(offset, 4096, |start, count| {
                            transferred += count;
                            Ok(black_box(encoded[start..start + count].to_vec()))
                        })
                        .unwrap(),
                    2 => {
                        transferred += envelope.len();
                        let opened = VerifiedIndex::open(&key, black_box(&envelope), id).unwrap();
                        opened
                            .read(offset, 4096, |start, count| {
                                transferred += count;
                                Ok(black_box(encoded[start..start + count].to_vec()))
                            })
                            .unwrap()
                    }
                    _ => unreachable!(),
                };
                times.push(start.elapsed().as_nanos() as u64);
                assert_eq!(result, plain[offset..offset + 4096]);
                checksum += black_box(result.iter().map(|b| u64::from(*b)).sum::<u64>());
            }
            times.sort_unstable();
            samples.push(json!({
                "round":round,
                "variant":(["whole_frame", "seek_cached_index", "seek_open_index_each_read"][variant]),
                "p50_ns":(times[49]+times[50])/2, "p99_ns":times[98],
                "simulated_response_bytes_per_read":transferred / 100,
                "checksum":checksum,
            }));
        }
    }
    assert_eq!(*blake3::hash(&encoded).as_bytes(), original_hash);
    let mut geometry = Vec::new();
    for granule in [4096, 16384, 65536, 262144, 1048576] {
        let proof = make_index(&key, &encoded, id, granule).unwrap();
        geometry.push(json!({
            "granule_bytes":granule,"index_envelope_bytes_per_4MiB_frame":proof.len(),
            "index_envelope_MiB_per_TiB":proof.len() as f64 / 4.0,
            "payload_reupload_bytes":0,
            "cached_index_range_bytes_for_aligned_4KiB":granule,
        }));
    }
    json!({
        "kind":"actual_legacy_encoder_authenticated_sidecar_seek_prototype",
        "positive_byte_exact_cases":positive,"negative_rejection_cases":negative,
        "test_key_only":true,"legacy_ciphertext_unchanged":true,
        "range_authentication_scope":"Selected sectors only; unrelated corruption and the legacy whole-frame tag are not revalidated by a partial read.",
        "timing_scope":"In-process CPU and memory copies, no filesystem/HTTP/Drive timing. Index cached versus opened each read are both shown.",
        "benchmark_reads_per_variant":900,"samples":samples,"geometry":geometry,
        "legacy_frame_bytes":encoded.len(),"index_bytes_64KiB":envelope.len(),
        "source_files":{
            "actual_aead_blake3":blake3::hash(include_bytes!("../../../../crates/mirage-crypto/src/aead.rs")).to_hex().to_string(),
            "actual_frame_blake3":blake3::hash(include_bytes!("../../../../crates/mirage-pack/src/encrypted_frame.rs")).to_hex().to_string(),
        },
        "existing_root_proof":crate::root_proof::run(&key),
    })
}
