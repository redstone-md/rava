use std::sync::Arc;

pub(super) struct RawField {
    pub(super) owner: String,
    pub(super) name: String,
    pub(super) descriptor: String,
}

#[derive(Clone)]
pub(super) struct ResolvedField {
    pub(super) owner: Arc<str>,
    pub(super) id: u32,
}
