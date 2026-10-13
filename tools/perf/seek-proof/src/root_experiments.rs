use super::*;

pub(super) fn run(key: &RepositoryKey) -> serde_json::Value {
    let mut lengths = vec![
        0,
        1,
        63,
        64,
        65,
        1023,
        1024,
        1025,
        2049,
        3071,
        3072,
        3073,
        4095,
        4096,
        4097,
        20487,
        65535,
        65536,
        65537,
        196607,
        196609,
        1048577,
        FRAME - 65,
        FRAME - 1,
        FRAME,
    ];
    let mut shape_rng = 470211_u64;
    for _ in 0..64 {
        shape_rng ^= shape_rng << 13;
        shape_rng ^= shape_rng >> 7;
        shape_rng ^= shape_rng << 17;
        lengths.push(1 + shape_rng as usize % FRAME);
    }
    let mut positive = 0;
    let mut rejected = 0;
    let mut standard_roots = 0;
    let mut rng = 883711_u64;
    for &length in &lengths {
        let (plain, encoded, id) = fixture(length, length as u64 + 333, key);
        let header = encoded[..PREFIX].try_into().unwrap();
        // The proof builder sees fully verified original plaintext once.
        assert_eq!(
            decode_encrypted_frame(key, &encoded, id.aad).unwrap(),
            plain
        );
        for granule in [4096, 65536, 262144] {
            let tree = (!plain.is_empty()).then(|| Tree::build(&plain, 0, granule));
            if let Some(tree) = &tree {
                tree.assert_standard_root(&plain);
                if tree.children.is_some() {
                    standard_roots += 1;
                }
            }
            let mut queries = vec![(0, 0), (length, 0), (0, length)];
            for boundary in [1, 64, 1024, 4096, 65536, 262144] {
                if boundary < length {
                    queries.push((boundary - 1, 2));
                }
            }
            for _ in 0..32 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let offset = rng as usize % (length + 1);
                let count = (rng.rotate_left(19) as usize % 200000).min(length - offset);
                queries.push((offset, count));
            }
            for (offset, count) in queries {
                let p = proofs(tree.as_ref(), offset, count, granule);
                let mut calls = 0;
                let got = read(
                    key,
                    id.aad,
                    granule,
                    offset,
                    count,
                    &header,
                    &p,
                    |start, size| {
                        calls += 1;
                        assert!(start >= PREFIX && start + size <= PREFIX + length);
                        Ok(encoded[start..start + size].to_vec())
                    },
                )
                .unwrap();
                assert_eq!(got, plain[offset..offset + count]);
                assert_eq!(calls, usize::from(count != 0));
                positive += 1;
            }
            for (offset, count) in [(length, 1), (usize::MAX, 1), (length + 1, 0)] {
                assert!(
                    read(
                        key,
                        id.aad,
                        granule,
                        offset,
                        count,
                        &header,
                        &[],
                        |_, _| panic!("invalid range fetch")
                    )
                    .is_err()
                );
                rejected += 1;
            }
        }
    }
    let (_, empty, empty_id) = fixture(0, 0, key);
    let mut invalid_empty_root = empty_id.aad;
    invalid_empty_root.plaintext_hash = PageHash::from_bytes([0x12; 32]);
    assert!(
        read(
            key,
            invalid_empty_root,
            65536,
            0,
            0,
            &empty[..PREFIX].try_into().unwrap(),
            &[],
            |_, _| panic!("empty read fetched bytes")
        )
        .is_err()
    );
    rejected += 1;
    let (plain, encoded, id) = fixture(FRAME, 412, key);
    let header: [u8; PREFIX] = encoded[..PREFIX].try_into().unwrap();
    let tree = Tree::build(&plain, 0, 65536);
    let proof = proofs(Some(&tree), 65536, 4096, 65536);
    for byte in 0..proof[0].len() * 32 {
        let mut corrupted = proof.clone();
        corrupted[0][byte / 32][byte % 32] ^= 1;
        assert!(
            read(
                key,
                id.aad,
                65536,
                65536,
                4096,
                &header,
                &corrupted,
                |start, size| Ok(encoded[start..start + size].to_vec())
            )
            .is_err()
        );
        rejected += 1;
    }
    for byte in 0..PREFIX {
        let mut corrupt_header = header;
        corrupt_header[byte] ^= 1;
        assert!(
            read(
                key,
                id.aad,
                65536,
                65536,
                4096,
                &corrupt_header,
                &proof,
                |start, size| Ok(encoded[start..start + size].to_vec())
            )
            .is_err()
        );
        rejected += 1;
    }
    for bit in 0..512 {
        assert!(
            read(
                key,
                id.aad,
                65536,
                65536,
                4096,
                &header,
                &proof,
                |start, size| {
                    let mut response = encoded[start..start + size].to_vec();
                    response[bit * 127 % size] ^= 1 << (bit % 8);
                    Ok(response)
                }
            )
            .is_err()
        );
        rejected += 1;
    }
    let mut wrong_root = id.aad;
    wrong_root.plaintext_hash = PageHash::from_bytes([0x99; 32]);
    assert!(
        read(
            key,
            wrong_root,
            65536,
            65536,
            4096,
            &header,
            &proof,
            |start, size| Ok(encoded[start..start + size].to_vec())
        )
        .is_err()
    );
    assert!(
        read(
            &RepositoryKey::from_bytes([0x99; 32]),
            id.aad,
            65536,
            65536,
            4096,
            &header,
            &proof,
            |start, size| Ok(encoded[start..start + size].to_vec())
        )
        .is_err()
    );
    rejected += 2;
    for delta in [-1_i64, 1, 65536] {
        assert!(
            read(
                key,
                id.aad,
                65536,
                65536,
                4096,
                &header,
                &proof,
                |start, size| {
                    let shifted = (start as i64 + delta) as usize;
                    Ok(encoded[shifted..shifted + size].to_vec())
                }
            )
            .is_err()
        );
        rejected += 1;
    }
    for malformed in [
        vec![],
        vec![vec![]],
        vec![vec![[0; 32]; 11]],
        vec![proof[0][..5].to_vec()],
        vec![proof[0].clone(), proof[0].clone()],
    ] {
        assert!(
            read(
                key,
                id.aad,
                65536,
                65536,
                4096,
                &header,
                &malformed,
                |start, size| Ok(encoded[start..start + size].to_vec())
            )
            .is_err()
        );
        rejected += 1;
    }
    for delta in [-1_i64, 1] {
        assert!(
            read(
                key,
                id.aad,
                65536,
                65536,
                4096,
                &header,
                &proof,
                |start, size| {
                    Ok(encoded[start..start + (size as i64 + delta) as usize].to_vec())
                }
            )
            .is_err()
        );
        rejected += 1;
    }
    let (_, other, other_id) = fixture(FRAME, 413, key);
    assert!(
        read(
            key,
            other_id.aad,
            65536,
            65536,
            4096,
            &header,
            &proof,
            |start, size| Ok(encoded[start..start + size].to_vec())
        )
        .is_err()
    );
    assert!(
        read(
            key,
            id.aad,
            65536,
            65536,
            4096,
            &header,
            &proof,
            |start, size| Ok(other[start..start + size].to_vec())
        )
        .is_err()
    );
    rejected += 2;
    let mut damaged_elsewhere = encoded.clone();
    damaged_elsewhere[PREFIX + FRAME - 1] ^= 1;
    *damaged_elsewhere.last_mut().unwrap() ^= 1;
    assert_eq!(
        read(
            key,
            id.aad,
            65536,
            65536,
            4096,
            &header,
            &proof,
            |start, size| Ok(damaged_elsewhere[start..start + size].to_vec())
        )
        .unwrap(),
        plain[65536..65536 + 4096]
    );
    positive += 1;

    // Refine a previously verified 64 KiB window into 4 KiB proofs without
    // downloading, decrypting, or retaining the other 63 coarse windows.
    let window_start = 3 * 65536;
    let coarse_path = tree.proof(window_start);
    let coarse = read(
        key,
        id.aad,
        65536,
        window_start,
        65536,
        &header,
        &[coarse_path.clone()],
        |start, size| Ok(encoded[start..start + size].to_vec()),
    )
    .unwrap();
    let fine = Tree::build(&coarse, window_start, 4096);
    let wrong_counter_tree = Tree::build(&coarse, 0, 4096);
    for i in 0..16 {
        let offset = window_start + i * 4096;
        let mut composed = fine.proof(offset);
        assert_eq!(composed.len(), 4);
        composed.extend_from_slice(&coarse_path);
        let got = read(
            key,
            id.aad,
            4096,
            offset,
            4096,
            &header,
            &[composed],
            |start, size| Ok(encoded[start..start + size].to_vec()),
        )
        .unwrap();
        assert_eq!(got, plain[offset..offset + 4096]);
        positive += 1;
        let mut invalid = wrong_counter_tree.proof(i * 4096);
        invalid.extend_from_slice(&coarse_path);
        assert!(
            read(
                key,
                id.aad,
                4096,
                offset,
                4096,
                &header,
                &[invalid],
                |start, size| Ok(encoded[start..start + size].to_vec())
            )
            .is_err()
        );
        rejected += 1;
    }
    let trees: Vec<_> = [4096, 65536, 262144]
        .iter()
        .map(|g| Tree::build(&plain, 0, *g))
        .collect();
    let mut samples = Vec::new();
    for round in 0..8 {
        for position in 0..4 {
            let variant = (round + position) % 4;
            let mut times = Vec::new();
            let mut wire_bytes = 0;
            let mut checksum = 0_u64;
            for iteration in 0..100 {
                let offset = ((iteration * 7919) % (FRAME / 4096)) * 4096;
                let start = Instant::now();
                let bytes = if variant == 0 {
                    wire_bytes += encoded.len();
                    let response = black_box(encoded.clone());
                    decode_encrypted_frame(key, &response, id.aad).unwrap()[offset..offset + 4096]
                        .to_vec()
                } else {
                    let granule = [4096, 65536, 262144][variant - 1];
                    let p = proofs(Some(&trees[variant - 1]), offset, 4096, granule);
                    wire_bytes += PREFIX + p.iter().map(|s| s.len() * 32).sum::<usize>();
                    read(
                        key,
                        id.aad,
                        granule,
                        offset,
                        4096,
                        &header,
                        &p,
                        |start, size| {
                            wire_bytes += size;
                            Ok(black_box(encoded[start..start + size].to_vec()))
                        },
                    )
                    .unwrap()
                };
                times.push(start.elapsed().as_nanos() as u64);
                assert_eq!(bytes, plain[offset..offset + 4096]);
                checksum += black_box(bytes.iter().map(|b| u64::from(*b)).sum::<u64>());
            }
            times.sort_unstable();
            samples.push(json!({"round":round,"variant":(["whole_frame", "root_proof_4KiB", "root_proof_64KiB", "root_proof_256KiB"][variant]),
                "p50_ns":(times[49]+times[50])/2,"p99_ns":times[98],"response_content_bytes_per_read":wire_bytes/100,"checksum":checksum}));
        }
    }
    let geometry: Vec<_> = [4096, 16384, 65536, 262144, 1048576]
        .into_iter()
        .map(|g| {
            let t = Tree::build(&plain, 0, g);
            let proof_bytes = t.proof(0).len() * 32;
            json!({"granule_bytes":g,"proof_bytes_for_one_sector":proof_bytes,
            "ciphertext_plus_proof_and_header_bytes":g+proof_bytes+PREFIX,
            "flat_leaf_CVs_plus_header_per_frame":FRAME/g*32+PREFIX,
            "flat_leaf_CVs_plus_header_MiB_per_TiB":(FRAME/g*32+PREFIX) as f64/4.0})
        })
        .collect();
    json!({"kind":"coarse_BLAKE3_proof_to_existing_plaintext_root_with_legacy_XChaCha20_seek",
        "new_trust_anchor_required":false,"payload_reupload_bytes":0,
        "proof_refinement":{"verified_window_bytes":65536,"fine_granule_bytes":4096,
            "fine_reads_checked":16,"additional_proof_bytes_per_selected_fine_leaf":128,
            "full_root_proof_bytes_per_fine_leaf":320,"full_frame_refetches_required":0},
        "positive_byte_exact_cases":positive,"negative_rejection_cases":rejected,
        "trees_compared_to_standard_blake3_hash":standard_roots,"length_cases":lengths,
        "benchmark_reads_per_variant":800,"samples":samples,"geometry":geometry,
        "scope":"Existing trusted root AND length are caller inputs. Proof bytes and legacy header are untrusted. Only selected plaintext is authenticated. No production format migration or network timing is claimed.",
        "cost_not_in_read_timing":"Initial complete authenticated read to build proof catalog; persistent catalog storage; HTTP RTT; publication/discovery; filesystem integration."})
}
