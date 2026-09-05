use mirage_types::PageHash;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseSpec {
    pub page_hash: PageHash,
    pub reason: String,
}
