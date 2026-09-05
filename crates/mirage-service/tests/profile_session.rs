use mirage_service::{ProfileLaunch, run_profile_session};
use std::time::Duration;
#[test]
fn launcher_must_be_inside_selected_root_before_etw_starts() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    let result = run_profile_session(&ProfileLaunch {
        game_root: root.path().to_owned(),
        launcher: outside.path().to_owned(),
        arguments: vec![],
        maximum_runtime: Duration::from_secs(1),
        drain_interval: Duration::ZERO,
        version_label: "v1".into(),
        configuration_label: "default".into(),
    });
    assert!(result.unwrap_err().message.contains("outside"));
}

#[cfg(windows)]
#[test]
fn maximum_runtime_terminates_and_reaps_the_launcher() {
    let root = tempfile::tempdir().unwrap();
    let system_root = std::env::var_os("WINDIR").unwrap();
    let launcher = root.path().join("ping.exe");
    std::fs::copy(
        std::path::Path::new(&system_root)
            .join("System32")
            .join("PING.EXE"),
        &launcher,
    )
    .unwrap();
    let result = run_profile_session(&ProfileLaunch {
        game_root: root.path().to_owned(),
        launcher,
        arguments: vec!["127.0.0.1".into(), "-n".into(), "6".into()],
        maximum_runtime: Duration::from_secs(1),
        drain_interval: Duration::ZERO,
        version_label: "v1".into(),
        configuration_label: "timeout".into(),
    });
    let result = match result {
        Ok(result) => result,
        Err(error) if error.message.contains("elevation") => return,
        Err(error) => panic!("unexpected profile error: {error}"),
    };
    assert!(result.timed_out);
    assert!(result.elapsed < Duration::from_secs(5));
}
