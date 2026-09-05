use mirage_ipc::*;
use mirage_types::RepositoryId;
#[test]
fn frame_roundtrip_correlation_and_version_are_exact() {
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 9,
        cancellation_id: Some(8),
        command: Command::RepositoryDetail {
            repository_id: RepositoryId::from_bytes([1; 16]),
        },
    };
    let frame = encode_frame(&request).unwrap();
    let decoded: Request = decode_frame(&frame).unwrap();
    assert_eq!(decoded, request);
    decoded.validate().unwrap();
    let response = Response {
        protocol_version: PROTOCOL_VERSION,
        request_id: decoded.request_id,
        body: ResponseBody::Accepted { operation_id: 7 },
    };
    assert_eq!(
        decode_frame::<Response>(&encode_frame(&response).unwrap())
            .unwrap()
            .request_id,
        9
    );
}
#[test]
fn bounds_malformed_unknown_and_auth_fail_closed() {
    let mut oversized = vec![0; MAX_FRAME_BYTES + 5];
    oversized[..4].copy_from_slice(&((MAX_FRAME_BYTES + 1) as u32).to_le_bytes());
    assert!(decode_frame::<Request>(&oversized).is_err());
    let bad = encode_frame(
        &serde_json::json!({"protocol_version":1,"request_id":1,"command":{"command":"unknown"}}),
    )
    .unwrap();
    assert!(decode_frame::<Request>(&bad).is_err());
    let principal = Principal {
        windows_sid: "S-1-5-21-test".into(),
        role: PrincipalRole::ReadOnly,
        authenticated: true,
    };
    assert!(Authorization::authorize(&principal, &Command::Status).is_ok());
    assert!(
        Authorization::authorize(
            &principal,
            &Command::UpdateBegin {
                repository_id: RepositoryId::from_bytes([1; 16])
            }
        )
        .is_err()
    );
}

#[test]
fn drive_access_token_roundtrips_but_debug_is_redacted() {
    let secret = "short-lived-access-token";
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 10,
        cancellation_id: None,
        command: Command::Materialize {
            repository_id: RepositoryId::from_bytes([2; 16]),
            capsule_id: mirage_types::CapsuleId::from_bytes([3; 16]),
            drive_access_token: Some(SensitiveString::new(secret.to_owned()).unwrap()),
            drive_quota: Some(DriveQuotaSnapshot {
                limit_bytes: Some(5 * 1024 * 1024 * 1024 * 1024),
                usage_bytes: 1024,
            }),
        },
    };
    assert!(!format!("{request:?}").contains(secret));
    let decoded: Request = decode_frame(&encode_frame(&request).unwrap()).unwrap();
    assert_eq!(decoded, request);
    assert!(SensitiveString::new("bad\ntoken".to_owned()).is_err());
    decoded.validate().unwrap();
}

#[test]
fn capacity_protocol_is_bounded_redacted_and_authorized_by_mutation() {
    let secret = "capacity-drive-token";
    let repository_id = RepositoryId::from_bytes([7; 16]);
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 11,
        cancellation_id: None,
        command: Command::CapacityAcquire {
            repository_id,
            requested_bytes: 64 * 1024 * 1024,
            lifetime_seconds: 3600,
            drive_access_token: Some(SensitiveString::new(secret.to_owned()).unwrap()),
        },
    };
    request.validate().unwrap();
    assert!(!format!("{request:?}").contains(secret));
    assert_eq!(
        decode_frame::<Request>(&encode_frame(&request).unwrap()).unwrap(),
        request
    );

    let read_only = Principal {
        windows_sid: "S-1-5-21-capacity".into(),
        role: PrincipalRole::ReadOnly,
        authenticated: true,
    };
    assert!(
        Authorization::authorize(
            &read_only,
            &Command::CapacityPlan {
                repository_id,
                requested_bytes: 1,
                drive_access_token: None,
            },
        )
        .is_ok()
    );
    assert!(Authorization::authorize(&read_only, &request.command).is_err());

    for (requested_bytes, lifetime_seconds) in [(0, 3600), (1, 29), (1_u64 << 50 | 1, 3600)] {
        let invalid = Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: 12,
            cancellation_id: None,
            command: Command::CapacityAcquire {
                repository_id,
                requested_bytes,
                lifetime_seconds,
                drive_access_token: None,
            },
        };
        assert!(invalid.validate().is_err());
    }
}
