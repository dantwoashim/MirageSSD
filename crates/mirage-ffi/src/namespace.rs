pub fn normalize(input: &[u16]) -> Option<Vec<u16>> {
    if input.contains(&0) {
        return None;
    }
    let text = String::from_utf16(input).ok()?;
    let normalized = text.replace('/', "\\").trim_matches('\\').to_lowercase();
    if normalized
        .split('\\')
        .any(|part| part == ".." || part == ".")
    {
        return None;
    }
    Some(normalized.encode_utf16().collect())
}
