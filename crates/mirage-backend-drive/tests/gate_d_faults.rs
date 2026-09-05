use mirage_backend::BackendErrorClass;
use mirage_backend_drive::{HttpResponse, error::classify_response};
use mirage_fault_inject::drive_scenarios::{DriveFault, DriveScenario};
use std::collections::BTreeMap;

#[test]
fn one_hundred_thousand_drive_faults_are_bounded_and_classified() {
    let mut scenario = DriveScenario::new(0x4d49_5241_4745);
    let mut counts = [0_u64; 5];
    for _ in 0..100_000 {
        match scenario.next_action() {
            DriveFault::Unauthorized => {
                counts[0] += 1;
                assert_eq!(classify(401, &[]).class, BackendErrorClass::Authentication);
            }
            DriveFault::RateLimited {
                retry_after_seconds,
            } => {
                counts[1] += 1;
                let error = classify(429, &[("retry-after", retry_after_seconds.to_string())]);
                assert_eq!(error.class, BackendErrorClass::RateLimit);
                assert_eq!(
                    error.retry_after.map(|value| value.as_secs()),
                    Some(retry_after_seconds as u64)
                );
            }
            DriveFault::Disconnect => counts[2] += 1,
            DriveFault::ShortBody => counts[3] += 1,
            DriveFault::Success => counts[4] += 1,
        }
    }
    assert!(counts[..4].iter().all(|count| *count > 0));
    assert_eq!(counts.iter().sum::<u64>(), 100_000);
    assert!(counts[..4].iter().sum::<u64>() < counts[4]);
}

fn classify(status: u16, headers: &[(&str, String)]) -> mirage_backend::BackendError {
    classify_response(&HttpResponse {
        status,
        headers: headers
            .iter()
            .cloned()
            .map(|(key, value)| (key.into(), value))
            .collect::<BTreeMap<_, _>>(),
        body: bytes::Bytes::new(),
    })
}
