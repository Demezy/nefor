use std::fs;
#[cfg(not(target_os = "macos"))]
use std::path::PathBuf;
use std::process::Command;

use nefor_cargo_test_harness::{prepare_paths_for_platform, verify_paths};

#[test]
fn platform_preparation_writes_exact_manifest() {
    let root = std::env::temp_dir().join(format!("nefor-signing-manifest-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    #[cfg(not(target_os = "macos"))]
    {
        let paths = vec![PathBuf::from("/does not need to exist")];
        prepare_paths_for_platform(&paths, &root).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("signed-executables.txt")).unwrap(),
            "/does not need to exist\n"
        );
    }
    #[cfg(target_os = "macos")]
    {
        let source = root.join("fixture.rs");
        let binary = root.join("fixture with spaces");
        fs::write(&source, "fn main() {}\n").unwrap();
        let status = Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .status()
            .unwrap();
        assert!(status.success());
        prepare_paths_for_platform(std::slice::from_ref(&binary), &root).unwrap();
        verify_paths(&[binary]).unwrap();
        let missing = root.join("missing executable");
        let error = nefor_cargo_test_harness::sign_and_verify(&[missing]).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }
    fs::remove_dir_all(root).unwrap();
}
