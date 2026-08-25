macro_rules! openai_provider_bin_tests {
    () => {
        mod tests {
            use super::*;

            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/support/openai_provider_bin_test_support.rs"
            ));
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/support/openai_provider_bin_default_tests.rs"
            ));
        }
    };
}

#[allow(dead_code)]
mod binary {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"));
    openai_provider_bin_tests!();
}
