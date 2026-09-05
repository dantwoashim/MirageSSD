//! Repository-scoped, content-addressed local implementation of `ObjectBackend`.

#![forbid(unsafe_code)]

mod backend;
mod layout;

pub use backend::LocalObjectBackend;
