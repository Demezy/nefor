pub(crate) fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_owned()
    } else {
        let cut = s.floor_char_boundary(n);
        format!("{}...<truncated {} bytes>", &s[..cut], s.len() - cut)
    }
}
