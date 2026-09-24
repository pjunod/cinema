use super::*;

#[derive(Clone)]
pub(super) struct ObsoleteEncodedGeneration {
    pub(super) key: String,
    pub(super) path: PathBuf,
}

pub(super) struct EncodedGenerationScanner {
    base: PathBuf,
    entries: Option<std::fs::ReadDir>,
}

impl EncodedGenerationScanner {
    pub(super) fn new(base: PathBuf) -> Self {
        Self {
            base,
            entries: None,
        }
    }

    pub(super) fn discover(
        &mut self,
        current_process: &str,
        scan_limit: usize,
        candidate_limit: usize,
        excluded: &HashSet<String>,
    ) -> Vec<ObsoleteEncodedGeneration> {
        if scan_limit == 0 || candidate_limit == 0 {
            return Vec::new();
        }
        if self.entries.is_none() {
            self.entries = match std::fs::read_dir(&self.base) {
                Ok(entries) => Some(entries),
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
                Err(error) => {
                    tracing::warn!(path = %self.base.display(), %error, "cannot discover obsolete encoded VOD generations");
                    return Vec::new();
                }
            };
        }

        let mut examined = 0;
        let mut candidates = Vec::new();
        while examined < scan_limit && candidates.len() < candidate_limit {
            let next = self.entries.as_mut().and_then(Iterator::next);
            let entry = match next {
                Some(Ok(entry)) => entry,
                Some(Err(error)) => {
                    examined += 1;
                    tracing::warn!(%error, "cannot inspect a VOD rendition directory entry");
                    continue;
                }
                None => {
                    // The following tick starts a fresh pass, which discovers
                    // encoded directories created after this iterator opened.
                    self.entries = None;
                    break;
                }
            };
            examined += 1;
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let marker = entry.path().join(ENCODED_PROCESS_NAME);
            let Ok(owner) = std::fs::read_to_string(&marker) else {
                // No marker means copy rendition (or unrelated directory). It
                // is never safe to infer encoded ownership from a hashed name.
                continue;
            };
            if owner.trim() == current_process {
                continue;
            }
            let key = entry.file_name().to_string_lossy().into_owned();
            if excluded.contains(&key) {
                continue;
            }
            candidates.push(ObsoleteEncodedGeneration {
                key,
                path: entry.path(),
            });
        }
        candidates
    }
}

pub(super) async fn publish_encoded_process_marker(path: &Path, process: &str) -> io::Result<()> {
    let marker = path.join(ENCODED_PROCESS_NAME);
    let temporary = path.join(format!("{ENCODED_PROCESS_NAME}.tmp"));
    tokio::fs::write(&temporary, format!("{process}\n")).await?;
    tokio::fs::rename(temporary, marker).await
}
