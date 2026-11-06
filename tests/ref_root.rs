use std::process::Command;

fn generate(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_openapi-ts-rs"))
        .arg("tests/fixtures/ref-root/nested/root.yml")
        .args(args)
        .output()
        .expect("openapi-ts-rs runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("utf-8 output")
}

#[test]
fn keys_external_files_relative_to_the_spec_directory_by_default() {
    let ts = generate(&["--strict"]);
    assert!(ts.contains(r#"external["../shared/pets.yml"]["components"]["schemas"]["Pet"]"#), "{ts}");
}

#[test]
fn keys_external_files_relative_to_the_ref_root() {
    let ts = generate(&["--strict", "--ref-root", "tests/fixtures/ref-root/shared"]);
    assert!(ts.contains(r#"external["pets.yml"]["components"]["schemas"]["Pet"]"#), "{ts}");
    assert!(!ts.contains("../shared/"), "{ts}");
}

#[test]
fn ref_root_only_renames_external_keys() {
    let default = generate(&["--strict"]);
    let rooted = generate(&["--strict", "--ref-root", "tests/fixtures/ref-root/shared"]);
    assert_eq!(default.replace("../shared/pets.yml", "pets.yml"), rooted);
}
