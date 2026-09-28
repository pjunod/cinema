use super::*;

pub(super) struct RollingTerminalAdmission {
    pub(super) manager: Arc<TranscodeManager>,
    pub(super) session_id: String,
    pub(super) session: Arc<Session>,
    pub(super) identity: RollingTerminalIdentity,
    pub(super) terminal_committer:
        Option<Arc<dyn crate::playback_control::TerminalControlCommitter>>,
}

impl crate::playback_control::RollingTerminalAdmission for RollingTerminalAdmission {
    fn accepted(&self, outcome: crate::playback_control::RollingControlOutcome) {
        if outcome.lease.terminal != Some(crate::playback_control::RollingTerminalCause::End) {
            return;
        }
        let (receipt, sender) = RollingTerminalResultReceipt::pending();
        let expires_at_unix_ms = Arc::new(AtomicI64::new(0));
        let exact_commit = Arc::new(std::sync::Mutex::new(None));
        let operation = RollingTerminalOperation {
            identity: self.identity.clone(),
            result: receipt,
            expires_at_unix_ms: Arc::clone(&expires_at_unix_ms),
            exact_commit: Arc::clone(&exact_commit),
        };
        let handoff = {
            let mut shared = self
                .session
                .terminal_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if shared.is_some() {
                return;
            }
            let handoff = crate::playback_control::TerminalResponseHandoff::new(Arc::clone(
                &self.session.terminal_response_pending,
            ));
            *shared = Some(operation.clone());
            handoff
        };
        self.manager
            .terminal_controls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(self.session_id.clone(), operation);
        let manager = Arc::clone(&self.manager);
        let session_id = self.session_id.clone();
        let session = Arc::clone(&self.session);
        let terminal_committer = self.terminal_committer.clone();
        tokio::spawn(async move {
            let result = manager
                .finish_hls_session_control(
                    session_id,
                    session,
                    outcome.disposition,
                    outcome.accepted_sequence,
                    outcome.action,
                    outcome.action_suppressed,
                    outcome.preparation_directive,
                    outcome.platform,
                    outcome.selection,
                    outcome.lease.expires_at_unix_ms(),
                    outcome.lease.timeout_ms(),
                    outcome.flow_ticket,
                    outcome.lease.terminal.map_or(
                        "active",
                        crate::playback_control::RollingTerminalCause::status,
                    ),
                    true,
                    Some(handoff),
                    terminal_committer,
                )
                .await;
            let prepared_commit = result
                .as_ref()
                .ok()
                .and_then(|result| result.terminal_commit.as_ref())
                .cloned();
            let exact_expiry = prepared_commit
                .as_ref()
                .and_then(|commit| commit.expires_at_unix_ms())
                // Tests and embedders may intentionally omit durable commit;
                // still start their bound after result preparation, never at
                // actor admission before the immutable result exists.
                .unwrap_or_else(|| {
                    crate::media_sessions::unix_ms()
                        .saturating_add(crate::playback_control::TERMINAL_ACK_REPLAY_TTL_MS)
                });
            *exact_commit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = prepared_commit;
            expires_at_unix_ms.store(exact_expiry, Release);
            let _ = sender.send(Some(result));
        });
    }
}
