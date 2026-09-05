pub const DRIVE_FILE: &str = "https://www.googleapis.com/auth/drive.file";

#[must_use]
pub fn requested_scopes() -> Vec<String> {
    vec![DRIVE_FILE.to_owned()]
}
