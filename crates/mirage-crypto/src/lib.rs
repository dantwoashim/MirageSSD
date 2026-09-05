//! Narrow platform cryptography boundary.
#![allow(unsafe_code)]
pub mod aead;
pub mod dpapi;
pub mod durable_file;
pub mod file_acl;
pub mod key_id;
pub mod key_store;
pub mod recovery;
pub mod repository_key_store;
pub mod signing;
