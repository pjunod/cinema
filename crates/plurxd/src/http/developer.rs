//! What the Developer settings section needs to say out loud.
//!
//! The Developer tab already lists, for each switch that costs something, what
//! must be true before turning it on. Those lists were static prose: an
//! operator read "every node must advertise its private cluster API address"
//! and then had no way to find out whether their nodes do. This route answers
//! the second half — for every prerequisite, whether the server can currently
//! observe it holding.
//!
//! Three rules shape it, and each one is a decision worth keeping:
//!
//! 1. **It is advisory. It gates nothing.** No enable path reads this route,
//!    and `an_unmet_prerequisite_does_not_block_the_switch` exists to keep
//!    that true. A prerequisite the server cannot see is not a reason to
//!    refuse an operator who can see it — they have the deployment in front of
//!    them and this process does not.
//! 2. **`Unobservable` is a real answer, not a failure.** A fleet
//!    qualification receipt lives in a CI artifact; the daemon has never seen
//!    it and never will. Reporting that honestly is more useful than a green
//!    tick that means "not checked", which is the failure mode this route
//!    exists to replace.
//! 3. **Evidence is what was read, not a restatement of the requirement.** A
//!    row saying "budgets are configured" teaches nothing; a row saying
//!    "chunk 30 s, transfer 1,200 s, install 120 s" lets the operator decide.

use axum::{extract::State, Json};
use serde::Serialize;

use super::error::ApiError;
use super::extract::AdminUser;
use crate::state::AppState;

/// Whether the server can see this prerequisite holding right now.
///
/// Deliberately three values. Two would force every fact the daemon cannot
/// reach into either a false "met" or a misleading "unmet", and both of those
/// are worse than saying so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RequirementStatus {
    /// Read from this process and holding.
    Met,
    /// Read from this process and not holding.
    Unmet,
    /// Not reachable from a running daemon at all. `evidence` says why and
    /// where the answer does live.
    Unobservable,
}

#[derive(Debug, Serialize)]
pub(crate) struct DeveloperRequirement {
    /// Stable across builds so the web tests and an operator's notes can name
    /// one row. Never reused for a different question.
    pub id: &'static str,
    pub title: &'static str,
    pub status: RequirementStatus,
    /// What this process actually read. One sentence, always populated.
    pub evidence: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct DeveloperEnableItem {
    pub id: &'static str,
    pub title: &'static str,
    /// The switch's current position, when the item has one. `None` means the
    /// capability has no switch — it is compiled in and behaves by itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    pub requirements: Vec<DeveloperRequirement>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DeveloperReadiness {
    /// Every row is a live read; nothing here is cached between requests.
    pub items: Vec<DeveloperEnableItem>,
}

/// `GET /api/v1/developer/readiness` — admin, read-only, advisory.
pub(crate) async fn readiness(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<DeveloperReadiness>, ApiError> {
    let settings = state.store.settings_snapshot().await.unwrap_or_default();
    let setting = |key: &str| settings.get(key).map(|value| value.trim().to_owned());
    let control_advertised =
        setting(plurx_core::store::keys::PLAYBACK_CONTROL_PROTOCOL_V1).as_deref() == Some("1");

    Ok(Json(DeveloperReadiness {
        items: vec![
            cluster_transport_recovery(&state).await,
            playback_control_protocol(control_advertised),
            prepared_quality_handoff(),
        ],
    }))
}

async fn cluster_transport_recovery(state: &AppState) -> DeveloperEnableItem {
    let budgets = &state.snapshot_recovery_budgets;
    let mut requirements = Vec::with_capacity(4);

    // The daemon can report its own budgets and that they passed startup
    // validation. It cannot know the state size they have to cover, so the
    // evidence names the numbers rather than pronouncing them sufficient.
    requirements.push(DeveloperRequirement {
        id: "recovery_budgets",
        title: "Recovery budgets fit the deployment",
        status: RequirementStatus::Met,
        evidence: format!(
            "This node: chunk {} s, whole transfer {} s, final install {} s — accepted by startup \
             validation, which also requires chunk ≤ transfer. Whether they cover your measured \
             state size is a deployment judgement the daemon cannot make.",
            budgets.chunk_secs, budgets.transfer_secs, budgets.install_secs
        ),
    });

    requirements.push(match state.membership.status().await {
        Ok(status) => {
            let missing = status
                .nodes
                .iter()
                .filter(|node| node.advertised_host.trim().is_empty())
                .map(|node| node.node_id.clone())
                .collect::<Vec<_>>();
            if missing.is_empty() {
                DeveloperRequirement {
                    id: "cluster_api_advertised",
                    title: "Authenticated diagnostics are reachable",
                    status: RequirementStatus::Met,
                    evidence: format!(
                        "All {} committed node(s) advertise a cluster API host. Keeping \
                         /cluster/transport/sqlite off the public Internet is a network fact the \
                         daemon cannot verify; the trusted network is configured as {}.",
                        status.nodes.len(),
                        describe_trusted_network(&budgets.trusted_network),
                    ),
                }
            } else {
                DeveloperRequirement {
                    id: "cluster_api_advertised",
                    title: "Authenticated diagnostics are reachable",
                    status: RequirementStatus::Unmet,
                    evidence: format!(
                        "{} of {} committed node(s) advertise no cluster API host: {}.",
                        missing.len(),
                        status.nodes.len(),
                        missing.join(", ")
                    ),
                }
            }
        }
        Err(error) => DeveloperRequirement {
            id: "cluster_api_advertised",
            title: "Authenticated diagnostics are reachable",
            status: RequirementStatus::Unobservable,
            evidence: format!(
                "This node is not serving replicated membership, so it holds no roster to check \
                 ({error})."
            ),
        },
    });

    requirements.push(
        match state.membership.cache_admin_revocation_ready().await {
            Ok(true) => DeveloperRequirement {
                id: "cache_revocation_capability",
                title: "Every committed node proves cache revocation",
                status: RequirementStatus::Met,
                evidence: "Every member of the exact committed configuration wrote the cache \
                       revocation capability inside its current heartbeat."
                    .to_owned(),
            },
            Ok(false) => DeveloperRequirement {
                id: "cache_revocation_capability",
                title: "Every committed node proves cache revocation",
                status: RequirementStatus::Unmet,
                evidence: "At least one committed member has not proven the cache revocation \
                       capability in its current heartbeat, so cache-only recovery \
                       authorization stays closed. Cluster → Nodes names which."
                    .to_owned(),
            },
            Err(error) => DeveloperRequirement {
                id: "cache_revocation_capability",
                title: "Every committed node proves cache revocation",
                status: RequirementStatus::Unobservable,
                evidence: format!("The local applied roster could not be read ({error})."),
            },
        },
    );

    // The 20+20 campaign is a `make cluster-transport-recovery-check`
    // artifact validated by plurx-cluster-check. No running daemon has ever
    // held it, and inventing a proxy for it here would be the exact dishonesty
    // this route replaces.
    requirements.push(DeveloperRequirement {
        id: "recovery_receipt",
        title: "The recovery receipt passes",
        status: RequirementStatus::Unobservable,
        evidence: "The real-TLS 3 MiB transport matrix and the twenty learner plus twenty voter \
                   recovery cycles are a qualification artifact from `make \
                   cluster-transport-recovery-check`. The daemon never receives it; read the \
                   run's receipt."
            .to_owned(),
    });

    DeveloperEnableItem {
        id: "cluster_transport_recovery",
        title: "Enable cluster transport recovery",
        enabled: None,
        requirements,
    }
}

fn playback_control_protocol(advertised: bool) -> DeveloperEnableItem {
    let vocabulary = crate::playback_control::control_vocabulary_snapshot();
    let complete = vocabulary.complete.iter().sum::<u64>();
    let partial = vocabulary.partial.iter().sum::<u64>();

    let reporters = if complete + partial == 0 {
        DeveloperRequirement {
            id: "clients_report",
            title: "Every client has a passive reporter",
            status: RequirementStatus::Unobservable,
            evidence: "No client has completed a control exchange with this process since it \
                       started, so it has nothing to report. These counters are process-local \
                       and reset on restart."
                .to_owned(),
        }
    } else if complete == 0 {
        DeveloperRequirement {
            id: "clients_report",
            title: "Every client has a passive reporter",
            status: RequirementStatus::Unmet,
            evidence: format!(
                "{partial} exchange(s) since start, none from a client declaring the full \
                 vocabulary. By platform (web/apple/android): partial {:?}.",
                vocabulary.partial
            ),
        }
    } else {
        DeveloperRequirement {
            id: "clients_report",
            title: "Every client has a passive reporter",
            status: RequirementStatus::Met,
            evidence: format!(
                "{complete} exchange(s) since start from clients declaring the full vocabulary, \
                 {partial} partial. By platform (web/apple/android): complete {:?}, partial {:?}.",
                vocabulary.complete, vocabulary.partial
            ),
        }
    };

    DeveloperEnableItem {
        id: "playback_control_protocol_v1",
        title: "Advertise playback control protocol v1",
        enabled: Some(advertised),
        requirements: vec![reporters],
    }
}

fn prepared_quality_handoff() -> DeveloperEnableItem {
    let staged = crate::playback_control::preparation_staged_snapshot();
    let vocabulary = crate::playback_control::control_vocabulary_snapshot();
    let complete = vocabulary.complete.iter().sum::<u64>();

    let mut requirements = Vec::with_capacity(3);

    // A build fact, and the honest answer is the one that changes when §4
    // lands rather than when an operator changes a setting. `Unmet` here is
    // this route reporting on the code it is compiled into.
    requirements.push(DeveloperRequirement {
        id: "server_preparation_is_real",
        title: "Server preparation is real",
        status: RequirementStatus::Unmet,
        evidence: "This build stages metadata only: a prepared successor publishes \
                   `encoder: staged` with no candidate worker behind its playlist, and no \
                   capacity is reserved for it."
            .to_owned(),
    });

    requirements.push(if complete > 0 {
        DeveloperRequirement {
            id: "client_two_player_handoff",
            title: "A client owns a measured two-player handoff",
            status: RequirementStatus::Unmet,
            evidence: format!(
                "{complete} exchange(s) since start declared `prepare_replacement`, but declaring \
                 the action is not the measured handoff: no shipped client aligns a second \
                 timeline with `media_origin_ms`, switches visibly, and then reports the switch. \
                 Preparations since start: {} staged, {} refused.",
                staged.staged, staged.refused
            ),
        }
    } else {
        DeveloperRequirement {
            id: "client_two_player_handoff",
            title: "A client owns a measured two-player handoff",
            status: RequirementStatus::Unmet,
            evidence: format!(
                "No client has declared `prepare_replacement` to this process since it started. \
                 Preparations since start: {} staged, {} refused.",
                staged.staged, staged.refused
            ),
        }
    });

    requirements.push(DeveloperRequirement {
        id: "fleet_receipt",
        title: "The fleet receipt is complete",
        status: RequirementStatus::Unobservable,
        evidence: "Twenty consecutive commits on the corrected realistic-runway instrument with \
                   stable memory is a fleet measurement. One node's counters cannot stand in for \
                   it."
        .to_owned(),
    });

    DeveloperEnableItem {
        id: "prepared_quality_handoff",
        title: "Enable prepared quality handoff",
        enabled: None,
        requirements,
    }
}

fn describe_trusted_network(trusted: &str) -> String {
    if trusted.trim().is_empty() {
        "unset".to_owned()
    } else {
        format!("`{}`", trusted.trim())
    }
}
