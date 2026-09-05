#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheFault {
    None,
    Reopen,
    Reconcile,
    CorruptClean,
}

#[must_use]
pub const fn fault_at(operation: u64) -> CacheFault {
    if operation != 0 && operation.is_multiple_of(997) {
        CacheFault::CorruptClean
    } else if operation != 0 && operation.is_multiple_of(251) {
        CacheFault::Reconcile
    } else if operation != 0 && operation.is_multiple_of(127) {
        CacheFault::Reopen
    } else {
        CacheFault::None
    }
}
