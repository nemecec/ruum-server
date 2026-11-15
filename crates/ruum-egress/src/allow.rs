//! `egress.allow` of the manifest (`contracts/manifests/app.schema.json`): host names, exact or
//! `*.` wildcards.

/// Whether an entry matches the schema's pattern `^(\*\.)?([a-z0-9-]+\.)+[a-z]{2,63}$`.
pub fn valid_entry(entry: &str) -> bool {
    let rest = entry.strip_prefix("*.").unwrap_or(entry);
    let labels: Vec<&str> = rest.split('.').collect();
    let Some((last, head)) = labels.split_last() else {
        return false;
    };
    !head.is_empty()
        && head.iter().all(|l| {
            !l.is_empty()
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        && (2..=63).contains(&last.len())
        && last.bytes().all(|b| b.is_ascii_lowercase())
}

/// Whether a host name (lowercase, no trailing dot: [`crate::addr::Host::Name`]) matches one
/// entry of the list. `*.example.com` matches one or more labels below `example.com`, never
/// `example.com` itself and never `evilexample.com`.
pub fn host_allowed(list: &[String], host: &str) -> bool {
    list.iter().any(|entry| match entry.strip_prefix("*.") {
        Some(suffix) => {
            host.len() > suffix.len() + 1
                && host.ends_with(suffix)
                && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
        }
        None => entry == host,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_follow_the_schema() {
        for ok in [
            "registry.npmjs.org",
            "*.example.com",
            "a-b.c0.io",
            "*.a.b.museum",
        ] {
            assert!(valid_entry(ok), "{ok}");
        }
        for bad in [
            "",
            "com",
            "*.com",
            "*",
            "*.",
            "example.c",
            "example.c0m",
            "Example.com",
            "a..com",
            "example.com.",
            "**.example.com",
            "a.*.example.com",
            "1.2.3.4",
            "under_score.com",
        ] {
            assert!(!valid_entry(bad), "{bad}");
        }
    }

    #[test]
    fn matching() {
        let list = vec!["api.example.com".to_string(), "*.example.org".to_string()];
        assert!(host_allowed(&list, "api.example.com"));
        assert!(!host_allowed(&list, "x.api.example.com"));
        assert!(!host_allowed(&list, "example.com"));
        assert!(host_allowed(&list, "a.example.org"));
        assert!(host_allowed(&list, "a.b.example.org"));
        assert!(!host_allowed(&list, "example.org"));
        assert!(!host_allowed(&list, "evilexample.org"));
        assert!(!host_allowed(&list, "example.org.evil.net"));
        assert!(!host_allowed(&[], "api.example.com"));
    }
}
