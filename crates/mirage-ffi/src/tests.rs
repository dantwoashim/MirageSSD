use super::safe_object_id;

#[test]
fn local_object_ids_are_single_safe_windows_components() {
    assert!(safe_object_id("pack-0123456789abcdef.bin"));
    for hostile in [
        "",
        "..",
        "../pack.bin",
        "sub\\pack.bin",
        "pack.bin/",
        "pack.bin\\",
        "C:pack.bin",
        "pack.bin:$DATA",
        "CON",
        "nul.bin",
        "pack.bin.",
        "pack.bin ",
    ] {
        assert!(
            !safe_object_id(hostile),
            "accepted hostile object ID {hostile:?}"
        );
    }
}
