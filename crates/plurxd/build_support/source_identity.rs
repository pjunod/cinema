//! Exact source identity for authenticated native replacement, independent of display labels.

fn full_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn resolve_source_commit(
    explicit: Option<&str>,
    description: &str,
    git_commit: impl FnOnce() -> Option<String>,
) -> Result<String, &'static str> {
    if let Some(value) = explicit {
        let value = value.trim();
        return if full_commit(value) {
            Ok(value.to_owned())
        } else {
            Err("PLURX_BUILD_SHA must be a full forty-character source commit")
        };
    }
    if description.ends_with("-dirty") {
        return Ok("unknown".into());
    }
    Ok(git_commit()
        .filter(|value| full_commit(value))
        .or_else(|| full_commit(description).then(|| description.to_owned()))
        .unwrap_or_else(|| "unknown".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn release_and_fixture_labels_never_replace_full_source_identity() {
        for label in ["v0.7.1", "dryrun-0123456", "qualification-local-only"] {
            assert_eq!(
                resolve_source_commit(Some(COMMIT), label, || panic!(
                    "explicit source identity wins"
                )),
                Ok(COMMIT.into())
            );
            assert_ne!(label, COMMIT);
        }
    }

    #[test]
    fn malformed_explicit_commit_cannot_fall_back_to_another_source() {
        for value in [
            "",
            "v0.7.1",
            "0123456",
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
        ] {
            assert!(resolve_source_commit(Some(value), COMMIT, || Some(COMMIT.into())).is_err());
        }
    }

    #[test]
    fn clean_checkout_uses_full_git_commit_beside_human_description() {
        assert_eq!(
            resolve_source_commit(None, "v0.7.0-12-g0123456", || Some(COMMIT.into())),
            Ok(COMMIT.into())
        );
    }

    #[test]
    fn dirty_checkout_cannot_claim_an_unchanged_candidate_commit() {
        assert_eq!(
            resolve_source_commit(None, "v0.7.0-dirty", || panic!(
                "dirty source has no exact identity"
            )),
            Ok("unknown".into())
        );
    }

    #[test]
    fn source_archive_can_use_an_explicit_full_commit_build_ref() {
        assert_eq!(
            resolve_source_commit(None, COMMIT, || None),
            Ok(COMMIT.into())
        );
    }

    #[test]
    fn missing_or_noncommit_identity_remains_unknown() {
        assert_eq!(
            resolve_source_commit(None, "v0.7.1", || Some("0123456".into())),
            Ok("unknown".into())
        );
    }
}
