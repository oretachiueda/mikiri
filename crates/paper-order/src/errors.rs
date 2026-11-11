pub fn mask(text: &str, account: Option<&str>) -> String {
    let text = match account.filter(|account| !account.is_empty()) {
        Some(account) => text.replace(account, "<account>"),
        None => text.to_owned(),
    };
    let chars: Vec<char> = text.chars().collect();
    let mut masked = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if matches!(chars[i], 'D' | 'U') && (i == 0 || !chars[i - 1].is_alphanumeric()) {
            let mut end = i + 1;
            while end < chars.len() && chars[end].is_ascii_digit() {
                end += 1;
            }
            if end > i + 1 && (end == chars.len() || !chars[end].is_alphanumeric()) {
                masked.push_str("<account>");
                i = end;
                continue;
            }
        }
        masked.push(chars[i]);
        i += 1;
    }
    masked
}

pub fn refusal(error: &anyhow::Error, account: Option<&str>) -> (String, String) {
    let chain = format!("{error:#}");
    let stderr = mask(&chain, account);
    let stdout = if chain.to_ascii_lowercase().contains("321")
        && chain.to_ascii_lowercase().contains("read-only")
    {
        "refused: TWS API is read-only".to_owned()
    } else {
        let first = mask(&error.to_string(), account);
        format!(
            "refused: {}",
            first
                .strip_prefix("refused: ")
                .unwrap_or(&first)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    (stdout, stderr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_known_accounts_and_standalone_account_tokens() {
        assert_eq!(
            mask("account ACCT::765; D321 U654", Some("ACCT::765")),
            "account <account>; <account> <account>"
        );
        assert_eq!(mask("D321 U654", None), "<account> <account>");
        assert_eq!(mask("", Some("")), "");
    }

    #[test]
    fn account_tokens_respect_unicode_alphanumeric_boundaries() {
        assert_eq!(
            mask("(D321) xD321 D321x éD321 D321界", None),
            "(<account>) xD321 D321x éD321 D321界"
        );
    }

    #[test]
    fn refusal_masks_full_multiline_error_chain_and_collapses_stdout() {
        let error =
            anyhow::anyhow!("account D321\nresponse ACCT-X").context("request failed for U654");
        let (stdout, stderr) = refusal(&error, Some("ACCT-X"));
        assert_eq!(stdout, "refused: request failed for <account>");
        assert!(stderr.contains("request failed for <account>"));
        assert!(stderr.contains("account <account>\nresponse <account>"));
        assert!(!stderr.contains("D321"));
        assert!(!stderr.contains("U654"));
        assert!(!stderr.contains("ACCT-X"));
    }

    #[test]
    fn existing_refusal_keeps_one_prefix() {
        let (line, _) = refusal(&anyhow::anyhow!("refused: not a paper account"), None);
        assert_eq!(line, "refused: not a paper account");
    }

    #[test]
    fn read_only_321_has_stable_stdout_but_masked_chain_stderr() {
        let error = anyhow::anyhow!("TWS returned error 321: API is READ-ONLY for D321");
        let (stdout, stderr) = refusal(&error, None);
        assert_eq!(stdout, "refused: TWS API is read-only");
        assert_eq!(
            stderr,
            "TWS returned error 321: API is READ-ONLY for <account>"
        );
    }
}
