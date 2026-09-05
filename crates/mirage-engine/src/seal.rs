use mirage_types::PageHash;
use std::collections::BTreeSet;
#[derive(Debug, Clone, Default)]
pub struct SealLeaseSet {
    pages: BTreeSet<PageHash>,
}
impl SealLeaseSet {
    #[must_use]
    pub fn from_pages(pages: impl IntoIterator<Item = PageHash>) -> Self {
        Self {
            pages: pages.into_iter().collect(),
        }
    }
    #[must_use]
    pub fn contains(&self, page: PageHash) -> bool {
        self.pages.contains(&page)
    }
}
