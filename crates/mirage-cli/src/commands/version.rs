use mirage_config::CONFIG_FORMAT_VERSION;
use mirage_types::MirageError;
use serde::Serialize;

use crate::output;

#[derive(Debug, Serialize)]
struct VersionInfo {
    cli_version: &'static str,
    rust_version: &'static str,
    build_hash: &'static str,
    target: &'static str,
    schemas: SchemaVersions,
}

#[derive(Debug, Serialize)]
struct SchemaVersions {
    cli_envelope: u32,
    service_config: u32,
    public_error: u32,
}

pub fn run(json: bool) -> Result<(), MirageError> {
    let info = VersionInfo {
        cli_version: env!("CARGO_PKG_VERSION"),
        rust_version: env!("MIRAGE_RUSTC_VERSION"),
        build_hash: env!("MIRAGE_BUILD_HASH"),
        target: env!("MIRAGE_TARGET"),
        schemas: SchemaVersions {
            cli_envelope: output::CLI_ENVELOPE_VERSION,
            service_config: CONFIG_FORMAT_VERSION,
            public_error: 1,
        },
    };
    if json {
        output::emit_success(&info)
    } else {
        println!(
            "mirage {} ({}; {}; {})",
            info.cli_version, info.build_hash, info.target, info.rust_version
        );
        println!(
            "schemas: cli-envelope={}, service-config={}, public-error={}",
            info.schemas.cli_envelope, info.schemas.service_config, info.schemas.public_error
        );
        Ok(())
    }
}
