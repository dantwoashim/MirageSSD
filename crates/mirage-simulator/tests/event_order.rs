use mirage_simulator::{Event, EventKind, NetworkModel, SimTime, Simulator};
use mirage_types::PageOrdinal;
use std::collections::BTreeSet;

fn event(request_id: u64) -> Event {
    Event {
        kind: EventKind::ReadArrival,
        page: PageOrdinal::from_u32(0),
        request_id,
    }
}

#[test]
fn simultaneous_events_are_sequence_stable_and_cancellation_is_exact() {
    let mut simulator = Simulator::default();
    simulator
        .schedule(SimTime::from_ns(5), event(10))
        .expect("schedule");
    simulator
        .schedule(SimTime::from_ns(5), event(11))
        .expect("schedule");
    simulator
        .schedule(SimTime::from_ns(5), event(12))
        .expect("schedule");
    simulator.cancel_request(11);
    assert_eq!(simulator.next_event().expect("first").event.request_id, 10);
    assert_eq!(simulator.next_event().expect("second").event.request_id, 12);
    assert!(simulator.next_event().is_none());
    assert!(simulator.schedule(SimTime::from_ns(4), event(13)).is_err());
}

#[test]
fn same_seed_bandwidth_sharing_and_one_page_miss_are_analytic() {
    let network = NetworkModel {
        base_latency_ns: 10_000_000,
        jitter_ns: 0,
        jitter_seed: 7,
        bandwidth_bytes_per_second: 100_000_000,
        max_concurrency: 4,
        fail_fetches: BTreeSet::new(),
    };
    let one = network.fetch(SimTime::ZERO, 1_000_000, 0, 1).expect("one");
    let two = network.fetch(SimTime::ZERO, 1_000_000, 0, 2).expect("two");
    assert_eq!(one.first_byte_at.as_ns(), 10_000_000);
    assert_eq!(one.complete_at.as_ns(), 20_000_000);
    assert_eq!(two.complete_at.as_ns(), 30_000_000);
    assert_eq!(
        network
            .fetch(SimTime::ZERO, 1_000_000, 8, 1)
            .expect("same seed"),
        network
            .fetch(SimTime::ZERO, 1_000_000, 8, 1)
            .expect("same seed")
    );
    assert_eq!(
        Simulator::default()
            .one_page_miss(SimTime::ZERO, 1_000_000, &network)
            .expect("miss"),
        20_000_000
    );
}
