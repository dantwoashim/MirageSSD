use mirage_types::ContentHash;
use std::collections::HashSet;

#[derive(Default, Clone)]
pub struct MarkSet(HashSet<ContentHash>);
impl MarkSet {
    pub fn mark(&mut self, object: ContentHash) {
        self.0.insert(object);
    }
    #[must_use]
    pub fn contains(&self, object: &ContentHash) -> bool {
        self.0.contains(object)
    }
}
