#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    SataSsd,
    Nvme,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Network {
    pub mbps: u16,
    pub latency_ms: u16,
    pub jitter_ms: u16,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatrixCase {
    pub storage: Storage,
    pub cache_gib: u16,
    pub network: Network,
    pub defender_enabled: bool,
    pub warm_cache: bool,
}

#[must_use]
pub fn platform_cases() -> Vec<MatrixCase> {
    let networks = [10, 25, 50, 100, 300].map(|mbps| Network {
        mbps,
        latency_ms: if mbps <= 25 { 80 } else { 30 },
        jitter_ms: if mbps <= 25 { 20 } else { 5 },
    });
    let mut cases = Vec::with_capacity(2 * 6 * 5 * 2 * 2);
    for storage in [Storage::SataSsd, Storage::Nvme] {
        for cache_gib in [10, 20, 30, 40, 60, 100] {
            for network in networks {
                for defender_enabled in [true, false] {
                    for warm_cache in [false, true] {
                        cases.push(MatrixCase {
                            storage,
                            cache_gib,
                            network,
                            defender_enabled,
                            warm_cache,
                        });
                    }
                }
            }
        }
    }
    cases
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matrix_is_complete_and_deterministic() {
        let first = platform_cases();
        assert_eq!(first.len(), 240);
        assert_eq!(first, platform_cases());
        assert!(
            first
                .iter()
                .any(|case| case.cache_gib == 10 && case.network.mbps == 10)
        );
        assert!(
            first
                .iter()
                .any(|case| case.cache_gib == 100 && case.network.mbps == 300)
        );
    }
}
