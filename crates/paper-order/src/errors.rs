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
    let order_id_preflight = error
        .chain()
        .any(|cause| cause.to_string() == "cannot initialize execution order IDs");
    let readonly = order_id_preflight
        && error.chain().any(|cause| {
            cause.downcast_ref::<ibapi::Error>().is_some_and(
                |error| matches!(error, ibapi::Error::Notice(notice) if notice.code == 321),
            )
        });
    let stdout = if readonly {
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
        let notice = ibapi::Notice {
            request_id: None,
            code: 321,
            message: "The API interface is currently in Read-Only mode for D321.".into(),
            error_time: None,
            advanced_order_reject_json: String::new(),
        };
        let error = anyhow::Error::new(ibapi::Error::Notice(notice))
            .context("cannot initialize execution order IDs");
        let (stdout, stderr) = refusal(&error, None);
        assert_eq!(stdout, "refused: TWS API is read-only");
        assert!(stderr.contains("cannot initialize execution order IDs"));
        assert!(stderr.contains("321"));
        assert!(stderr.contains("<account>"));
        assert!(!stderr.contains("D321"));
    }

    #[test]
    fn validation_321_outside_order_id_preflight_is_not_read_only() {
        let error = anyhow::Error::new(ibapi::Error::Notice(ibapi::Notice {
            request_id: None,
            code: 321,
            message: "Invalid contract query".into(),
            error_time: None,
            advanced_order_reject_json: String::new(),
        }))
        .context("failed to read positions");
        assert_eq!(refusal(&error, None).0, "refused: failed to read positions");
    }

    #[test]
    fn unrelated_text_with_321_and_read_only_is_not_special_cased() {
        let error = anyhow::anyhow!("unrelated 321 read-only diagnostic");
        let (stdout, _) = refusal(&error, None);
        assert_eq!(stdout, "refused: unrelated 321 read-only diagnostic");
    }
}
