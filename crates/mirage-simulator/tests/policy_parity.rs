use mirage_cache::{PolicyCore, PolicyEvent, PolicyKind};
use mirage_simulator::cache_adapter::SimCachePolicy;

#[test]
fn live_and_simulator_adapters_choose_identical_victims() {
    for kind in [
        PolicyKind::Lru,
        PolicyKind::SegmentedLru,
        PolicyKind::TinyLfuHybrid,
    ] {
        let mut live = PolicyCore::new(kind, 3).expect("live");
        let mut sim = SimCachePolicy::new(kind, 3).expect("sim");
        let events = [
            PolicyEvent::Access {
                id: 1,
                blocking: false,
                prefetched: false,
            },
            PolicyEvent::Access {
                id: 2,
                blocking: false,
                prefetched: true,
            },
            PolicyEvent::Access {
                id: 3,
                blocking: false,
                prefetched: false,
            },
            PolicyEvent::Access {
                id: 1,
                blocking: false,
                prefetched: false,
            },
            PolicyEvent::Pin(1),
            PolicyEvent::Access {
                id: 4,
                blocking: true,
                prefetched: false,
            },
            PolicyEvent::Unpin(1),
            PolicyEvent::Access {
                id: 5,
                blocking: false,
                prefetched: true,
            },
        ];
        for event in events {
            assert_eq!(
                live.apply(event).expect("live event"),
                sim.apply(event).expect("sim event")
            );
        }
        assert_eq!(live.residents(), sim.residents());
    }
}
