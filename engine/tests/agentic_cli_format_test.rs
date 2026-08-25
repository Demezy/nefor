mod support;

use support::agentic_cli::truncate;

#[test]
fn truncate_does_not_panic_on_multibyte_boundary() {
    let s = "привет world hello мир — еще немного текста";
    for n in 0..=s.len() {
        let out = truncate(s, n);
        let head_end = out.find("...<truncated ").unwrap_or(out.len());
        let head = &out[..head_end];
        assert!(s.starts_with(head), "head must be a prefix of input");
        assert!(
            head.len() <= n,
            "head bytes ({}) must not exceed cap ({}) for n={}",
            head.len(),
            n,
            n
        );
    }
}
