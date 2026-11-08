//! Operator-facing explanation of why this Cinema can or cannot reach another
//! Cinema's private sharing listener. A diagnosis is an observation: the
//! ordinary pinned connection decides `connected`, and nothing here relaxes a
//! pin, admits a peer or persists anything.
use crate::{sharing_client::PeerError, sharing_host::VisiblePeer, state::AppState};
use plurx_core::{
    sharing::{Endpoint, SharingIdentity},
    sharing_tls::{dial_numeric_peer_classified, DialFailure},
};
use serde::Serialize;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

/// One pinned dial to one approved numeric address.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Attempt {
    pub ts_fqdn: String,
    pub address: String,
    /// `connected`, or the [`DialFailure`] stage that refused.
    pub result: &'static str,
    pub elapsed_ms: u64,
    /// Whether this host's Tailscale netmap contains the address. Absent when
    /// the host cannot be observed (for example from inside a container).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_online: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Diagnosis {
    pub connected: bool,
    /// A fixed outcome code; see [`Outcome`].
    pub outcome: &'static str,
    pub summary: String,
    pub next_step: String,
    pub checked_at_ms: i64,
    pub elapsed_ms: u64,
    /// The source machine the summary is about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// This Cinema's own Tailscale account, when the host could be observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub this_server: Option<crate::sharing_host::TailscaleIdentity>,
    pub attempts: Vec<Attempt>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Connected,
    IdentityMismatch,
    AuthFailed,
    ProtocolUnsupported,
    InvalidResponse,
    CertificateMismatch,
    Refused,
    NoPrivateSource,
    NotShared,
    PeerOffline,
    Blocked,
    NoAnswer,
    TlsFailed,
    Unavailable,
}

impl Outcome {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::IdentityMismatch => "identity_mismatch",
            Self::AuthFailed => "auth_failed",
            Self::ProtocolUnsupported => "protocol_unsupported",
            Self::InvalidResponse => "invalid_response",
            Self::CertificateMismatch => "certificate_mismatch",
            Self::Refused => "refused",
            Self::NoPrivateSource => "no_private_source",
            Self::NotShared => "not_shared",
            Self::PeerOffline => "peer_offline",
            Self::Blocked => "blocked",
            Self::NoAnswer => "no_answer",
            Self::TlsFailed => "tls_failed",
            Self::Unavailable => "unavailable",
        }
    }
}

fn failure_code(failure: DialFailure) -> &'static str {
    match failure {
        DialFailure::InvalidTarget => "invalid_target",
        DialFailure::NoPrivateSource => "no_private_source",
        DialFailure::TimedOut => "timed_out",
        DialFailure::Refused => "refused",
        DialFailure::Unreachable => "unreachable",
        DialFailure::ConnectFailed => "connect_failed",
        DialFailure::CertificateMismatch => "certificate_mismatch",
        DialFailure::TlsFailed => "tls_failed",
    }
}

/// The decision, separated from I/O so every branch is testable.
pub(crate) fn classify(
    verified: &Result<(), PeerError>,
    attempts: &[(Option<DialFailure>, Option<bool>, Option<bool>)],
) -> Outcome {
    match verified {
        Ok(()) => return Outcome::Connected,
        Err(PeerError::IdentityMismatch) => return Outcome::IdentityMismatch,
        Err(PeerError::Authentication) => return Outcome::AuthFailed,
        Err(PeerError::ProtocolUnsupported) => return Outcome::ProtocolUnsupported,
        Err(PeerError::InvalidResponse) => return Outcome::InvalidResponse,
        Err(_) => {}
    }
    let failures: Vec<_> = attempts.iter().filter_map(|(f, ..)| *f).collect();
    if attempts.iter().any(|(f, ..)| f.is_none()) {
        // A pinned TLS stream opened, yet the identity exchange did not finish.
        return Outcome::Unavailable;
    }
    if failures.contains(&DialFailure::CertificateMismatch) {
        return Outcome::CertificateMismatch;
    }
    if failures.contains(&DialFailure::Refused) {
        return Outcome::Refused;
    }
    if failures.contains(&DialFailure::TlsFailed) {
        return Outcome::TlsFailed;
    }
    if !failures.is_empty() && failures.iter().all(|f| *f == DialFailure::NoPrivateSource) {
        return Outcome::NoPrivateSource;
    }
    let silent = |f: &DialFailure| {
        matches!(
            f,
            DialFailure::TimedOut | DialFailure::Unreachable | DialFailure::ConnectFailed
        )
    };
    if failures.iter().any(silent) {
        let observed: Vec<_> = attempts
            .iter()
            .filter(|(f, ..)| f.as_ref().is_some_and(silent))
            .map(|(_, visible, online)| (*visible, *online))
            .collect();
        if observed.iter().all(|(visible, _)| *visible == Some(false)) {
            return Outcome::NotShared;
        }
        if observed
            .iter()
            .any(|(visible, online)| *visible == Some(true) && *online == Some(false))
        {
            return Outcome::PeerOffline;
        }
        if observed
            .iter()
            .any(|(visible, online)| *visible == Some(true) && *online != Some(false))
        {
            return Outcome::Blocked;
        }
        return Outcome::NoAnswer;
    }
    Outcome::Unavailable
}

fn account_phrase(identity: Option<&crate::sharing_host::TailscaleIdentity>) -> String {
    match identity.and_then(|i| i.login_name.as_deref().or(i.tailnet.as_deref())) {
        Some(account) => format!("the Tailscale account {account}"),
        None => "the Tailscale account this Cinema's host is signed in to".to_owned(),
    }
}

pub(crate) fn explain(
    outcome: Outcome,
    source: &str,
    identity: Option<&crate::sharing_host::TailscaleIdentity>,
    host_detail: &str,
) -> (String, String) {
    let account = account_phrase(identity);
    let (summary, next) = match outcome {
        Outcome::Connected => (
            format!("Connected to {source} and verified its Cinema identity."),
            String::new(),
        ),
        Outcome::IdentityMismatch => (
            format!("{source} answered as a different Cinema than the one in the invitation."),
            "Ask the other owner to check their Sharing addresses and send a fresh invitation.".to_owned(),
        ),
        Outcome::AuthFailed => (
            format!("{source} refused this Cinema's sharing credential."),
            "The other owner may have revoked or replaced this connection. Ask them for a fresh invitation, then use Re-pair.".to_owned(),
        ),
        Outcome::ProtocolUnsupported => (
            format!("{source} runs a Cinema version that cannot pair with this one."),
            "Update both Cinema servers, then check again.".to_owned(),
        ),
        Outcome::InvalidResponse => (
            format!("{source} answered, but not with a valid Cinema sharing response."),
            "Ask the other owner to check that port 32443 on that machine forwards to Cinema's sharing listener.".to_owned(),
        ),
        Outcome::CertificateMismatch => (
            format!("Reached {source}, but its Cinema certificate is not the one this connection trusts."),
            "The other Cinema's address list may point at a different machine, or its certificate was replaced. Ask the other owner for a fresh invitation.".to_owned(),
        ),
        Outcome::Refused => (
            format!("{source} is reachable over Tailscale, but nothing accepts Cinema connections on its sharing port."),
            "On the other Cinema, Settings → Sharing → \"Other Cinemas can reach this server\" must be ready (Tailscale Serve forwarding port 32443 to Cinema).".to_owned(),
        ),
        Outcome::TlsFailed => (
            format!("Reached {source}, but the secure connection did not complete."),
            "Something other than Cinema may be listening on that port. Ask the other owner to check their Sharing setup.".to_owned(),
        ),
        Outcome::NoPrivateSource => (
            "This Cinema has no private Tailscale address to connect from, so it cannot reach any other Cinema yet.".to_owned(),
            if host_detail.is_empty() {
                "Fix \"Tailscale on this node\" in Connection status above; Cinema retries automatically.".to_owned()
            } else {
                format!("Fix \"Tailscale on this node\" in Connection status above ({host_detail}); Cinema retries automatically.")
            },
        ),
        Outcome::NotShared => (
            format!("{source} is not in this Cinema's Tailscale network, so this Cinema cannot reach it."),
            format!("Open the invitation's Tailscale link and accept it while signed in to {account}. If your browser is signed in to Tailscale as someone else (for example the person who sent the invitation), sign out first or use a private window. The other owner can see on their Sharing page whether the share was accepted, and by whom."),
        ),
        Outcome::PeerOffline => (
            format!("{source} is in this Cinema's Tailscale network but is offline."),
            "The other Cinema's machine may be asleep, off or disconnected from Tailscale. Check again when it is online.".to_owned(),
        ),
        Outcome::Blocked => (
            format!("{source} is in this Cinema's Tailscale network and online, but did not answer on its sharing port."),
            "The other owner's Tailscale access rules may block shared users from that port, or the machine is not forwarding it. Ask them to check their Sharing page.".to_owned(),
        ),
        Outcome::NoAnswer => (
            format!("{source} did not answer over Tailscale."),
            format!("Most often the invitation's Tailscale share has not been accepted by {account}, or the other machine is offline. Open the invitation's Tailscale link while signed in to that account; the other owner's Sharing page shows whether the share was accepted."),
        ),
        Outcome::Unavailable => (
            format!("Could not complete a verified connection to {source}."),
            "Check again in a moment. If it keeps failing, check Connection status on both Cinemas.".to_owned(),
        ),
    };
    (summary, next)
}

/// Run the ordinary pinned connection and, beside it, one classified dial
/// per approved numeric address, plus this host's netmap when observable.
pub(crate) async fn diagnose(
    state: &AppState,
    endpoints: &[Endpoint],
    expected: &SharingIdentity,
) -> Diagnosis {
    diagnose_with(state, endpoints, expected, None).await
}

/// Explain a pinned connection that the caller already saw fail, without
/// paying for a second identity exchange.
pub(crate) async fn diagnose_failure(
    state: &AppState,
    endpoints: &[Endpoint],
    expected: &SharingIdentity,
    error: &PeerError,
) -> Diagnosis {
    diagnose_with(state, endpoints, expected, Some(clone_error(error))).await
}

/// Each diagnosis spawns a host CLI call and up to eight pinned dials; the
/// admin surfaces that run them are few, so a small bound queues the rest.
static DIAGNOSES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

async fn diagnose_with(
    state: &AppState,
    endpoints: &[Endpoint],
    expected: &SharingIdentity,
    known: Option<PeerError>,
) -> Diagnosis {
    let _permit = DIAGNOSES.acquire().await;
    let started = Instant::now();
    let manager = &state.sharing;
    let egress = manager.egress();
    let verified = async move {
        if let Some(error) = known {
            return Err(error);
        }
        crate::sharing_client::PeerConnection::verified(manager, endpoints, expected)
            .await
            .map(|(peer, _)| peer.verified_endpoint().map(|e| e.ts_fqdn.clone()))
    };
    let targets: Vec<(String, SocketAddr, String)> = endpoints
        .iter()
        .take(4)
        .flat_map(|endpoint| {
            std::iter::once(IpAddr::V4(endpoint.ipv4))
                .chain(endpoint.ipv6.map(IpAddr::V6))
                .map(move |ip| {
                    (
                        endpoint.ts_fqdn.clone(),
                        SocketAddr::new(ip, endpoint.port),
                        endpoint.spki_sha256.clone(),
                    )
                })
        })
        .collect();
    let dials = futures_util::future::join_all(targets.iter().map(|(name, address, pin)| {
        let egress = &egress;
        async move {
            let begun = Instant::now();
            let result = dial_numeric_peer_classified(*address, name, pin, egress)
                .await
                .err();
            (result, begun.elapsed())
        }
    }));
    let peers = async {
        if manager.native_host() {
            tokio::time::timeout(Duration::from_secs(4), crate::sharing_host::visible_peers())
                .await
                .ok()
                .flatten()
        } else {
            None
        }
    };
    let (verified, dials, peers): (_, _, Option<HashMap<IpAddr, VisiblePeer>>) =
        tokio::join!(verified, dials, peers);
    let mut classified = Vec::with_capacity(targets.len());
    let mut attempts = Vec::with_capacity(targets.len());
    for ((name, address, _), (failure, elapsed)) in targets.iter().zip(dials) {
        let peer = peers.as_ref().map(|p| p.get(&address.ip()));
        // A sharee-only entry can connect in but not be dialled: for this
        // direction the source is not in this tailnet.
        let visible = peer.map(|p| p.is_some_and(|p| p.dialable));
        let peer_online = peer.flatten().and_then(|p| p.online);
        classified.push((failure, visible, peer_online));
        attempts.push(Attempt {
            ts_fqdn: name.clone(),
            address: address.to_string(),
            result: failure.map_or("connected", failure_code),
            elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            visible,
            peer_online,
        });
    }
    let reached = verified.as_ref().ok().cloned().flatten();
    let outcome = classify(
        &verified.as_ref().map(|_| ()).map_err(clone_error),
        &classified,
    );
    let status = manager.status();
    let identity = status.host_setup.tailscale.clone();
    let source = reached
        .or_else(|| endpoints.first().map(|e| e.ts_fqdn.clone()))
        .unwrap_or_else(|| "the other Cinema".to_owned());
    let (summary, next_step) = explain(
        outcome,
        &source,
        identity.as_ref(),
        &status.host_setup.detail,
    );
    Diagnosis {
        connected: outcome == Outcome::Connected,
        outcome: outcome.code(),
        summary,
        next_step,
        checked_at_ms: crate::state::clock_ms(),
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        source: Some(source),
        this_server: identity,
        attempts,
    }
}

fn clone_error(error: &PeerError) -> PeerError {
    match error {
        PeerError::Unavailable => PeerError::Unavailable,
        PeerError::IdentityMismatch => PeerError::IdentityMismatch,
        PeerError::ProtocolUnsupported => PeerError::ProtocolUnsupported,
        PeerError::Authentication => PeerError::Authentication,
        PeerError::InvalidResponse => PeerError::InvalidResponse,
        PeerError::Rejected(status) => PeerError::Rejected(*status),
        PeerError::DolbyVisionUnsupported => PeerError::DolbyVisionUnsupported,
        PeerError::Busy { retry_after_secs } => PeerError::Busy {
            retry_after_secs: *retry_after_secs,
        },
    }
}

/// One compact log line per diagnosis: outcome, elapsed and each address's
/// stage. No credentials, invitation secrets or consent links exist here.
pub(crate) fn log(diagnosis: &Diagnosis, context: &str, id: &uuid::Uuid) {
    let attempts = diagnosis
        .attempts
        .iter()
        .map(|a| {
            format!(
                "{}={}{}",
                a.address,
                a.result,
                match a.visible {
                    Some(false) => " (not in this tailnet)",
                    Some(true) if a.peer_online == Some(false) => " (in this tailnet, offline)",
                    _ => "",
                }
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let source = diagnosis.source.as_deref().unwrap_or("");
    if diagnosis.connected {
        tracing::info!(context, %id, source, elapsed_ms = diagnosis.elapsed_ms, "sharing connection check succeeded");
    } else {
        tracing::warn!(
            context,
            %id,
            source,
            outcome = diagnosis.outcome,
            elapsed_ms = diagnosis.elapsed_ms,
            attempts = %attempts,
            "sharing connection check failed: {}",
            diagnosis.summary
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DialFailure as F;

    #[test]
    fn verified_peer_errors_take_precedence_over_dial_stages() {
        let dials = [(Some(F::TimedOut), Some(false), None)];
        assert_eq!(classify(&Ok(()), &dials), Outcome::Connected);
        assert_eq!(
            classify(&Err(PeerError::IdentityMismatch), &dials),
            Outcome::IdentityMismatch
        );
        assert_eq!(
            classify(&Err(PeerError::Authentication), &dials),
            Outcome::AuthFailed
        );
    }

    #[test]
    fn a_source_missing_from_this_tailnet_reads_as_an_unaccepted_share() {
        // The cross-account failure: the receiver's Tailscale never received
        // the device share, so the source's address is not in its netmap.
        assert_eq!(
            classify(
                &Err(PeerError::Unavailable),
                &[
                    (Some(F::TimedOut), Some(false), None),
                    (Some(F::TimedOut), Some(false), None)
                ]
            ),
            Outcome::NotShared
        );
        assert_eq!(
            classify(
                &Err(PeerError::Unavailable),
                &[(Some(F::TimedOut), None, None)]
            ),
            Outcome::NoAnswer
        );
        assert_eq!(
            classify(
                &Err(PeerError::Unavailable),
                &[(Some(F::TimedOut), Some(true), Some(false))]
            ),
            Outcome::PeerOffline
        );
        assert_eq!(
            classify(
                &Err(PeerError::Unavailable),
                &[(Some(F::TimedOut), Some(true), Some(true))]
            ),
            Outcome::Blocked
        );
    }

    #[test]
    fn concrete_stages_outrank_silence() {
        let unavailable = Err(PeerError::Unavailable);
        assert_eq!(
            classify(
                &unavailable,
                &[
                    (Some(F::TimedOut), Some(false), None),
                    (Some(F::Refused), None, None)
                ]
            ),
            Outcome::Refused
        );
        assert_eq!(
            classify(
                &unavailable,
                &[
                    (Some(F::Refused), None, None),
                    (Some(F::CertificateMismatch), None, None)
                ]
            ),
            Outcome::CertificateMismatch
        );
        assert_eq!(
            classify(
                &unavailable,
                &[
                    (Some(F::NoPrivateSource), None, None),
                    (Some(F::NoPrivateSource), None, None)
                ]
            ),
            Outcome::NoPrivateSource
        );
        assert_eq!(
            classify(&unavailable, &[(None, Some(true), Some(true))]),
            Outcome::Unavailable
        );
    }

    #[test]
    fn unaccepted_share_guidance_names_this_servers_account() {
        let identity = crate::sharing_host::TailscaleIdentity {
            login_name: Some("owner-b@example.com".into()),
            tailnet: Some("owner-b@example.com".into()),
            dns_name: "cinema-b.tail222.ts.net".into(),
            ipv4: "100.64.2.2".parse().expect("fixture"),
        };
        let (summary, next) = explain(
            Outcome::NotShared,
            "cinema-a.tail111.ts.net",
            Some(&identity),
            "",
        );
        assert!(summary.contains("cinema-a.tail111.ts.net"));
        assert!(next.contains("owner-b@example.com"), "{next}");
        assert!(next.contains("sign out"), "{next}");
        let (_, next) = explain(
            Outcome::NoPrivateSource,
            "x",
            None,
            "`tailscale status --json` did not answer",
        );
        assert!(next.contains("did not answer"), "{next}");
    }
}
