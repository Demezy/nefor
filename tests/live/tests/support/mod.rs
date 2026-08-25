pub fn require_explicit_capability() {
    assert_eq!(
        std::env::var("NEFOR_LIVE_TEST_CAPABILITY").as_deref(),
        Ok("explicit"),
        "live prerequisite blocked: invoke the repository-owned `just test-live` command"
    );
}
