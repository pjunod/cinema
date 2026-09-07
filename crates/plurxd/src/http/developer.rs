//! What the Developer settings section needs to say out loud.
//!
//! The Developer tab already lists, for each switch that costs something, what
//! must be true before turning it on. Those lists were static prose: an
//! operator read "every node must advertise its private cluster API address"
//! and then had no way to find out whether their nodes do. This route answers
//! the second half — for every prerequisite, what this server can currently
//! observe about it.
//!
//! Three rules shape it, and each one is a decision worth keeping:
//!
//! 1. **It is advisory. It gates nothing.** No enable path reads this route,
//!    and `an_unmet_prerequisite_does_not_block_the_switch` exists to keep
//!    that true. A prerequisite the server cannot see is not a reason to
//!    refuse an operator who can see it — they have the deployment in front of
//!    them and this process does not.
//! 2. **`Unobservable` is a real answer, and it is the common one.** A fleet
//!    qualification receipt lives in a CI artifact the daemon has never held.
//!    A single-node install has no committed roster. A counter that can see
//!    the clients which *did* report cannot speak for the ones which did not.
//!    Every one of those is `Unobservable`, because a green tick meaning "not
//!    checked" is the failure mode this route exists to replace, and answering
//!    `Met` from a fail-open default would be that tick with extra steps.
//! 3. **Evidence is what was read, and it names what was not read.** A row
//!    saying "budgets are configured" teaches nothing; a row saying "chunk
//!    30 s, transfer 1,200 s, install 120 s, and nothing here knows your state
//!    size" lets the operator decide.
//!
//! Rule 2 is why few rows can ever be `Met`. That is the honest shape of these
//! questions, not a gap to be closed by relaxing a threshold.

use axum::{extract::State, Json};
use plurx_core::cluster::membership::MembershipError;
use serde::Serialize;

use super::error::ApiError;
use super::extract::AdminUser;
use crate::state::AppState;

/// What the server can see of this prerequisite right now.
///
/// Deliberately three values. Two would force every fact the daemon cannot
/// reach into either a false `Met` or a misleading `Unmet`, and both of those
/// are worse than saying so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RequirementStatus {
    /// Read from this process, and what was read satisfies the requirement.
    Met,
    /// Read from this process, and what was read contradicts the requirement.
    Unmet,
    /// Not answerable from a running daemon, or answerable only in part.
    /// `evidence` says what was read, what was not, and where the rest lives.
    Unobservable,
}

#[derive(Debug, Serialize)]
pub(crate) struct DeveloperRequirement {
    /// Stable across builds so the web rows and an operator's notes can name
    /// one question. Never reused for a different one.
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
    /// Nothing here is cached between requests. Most rows are read when the
    /// request arrives; three of them (`recovery_receipt`, `fleet_receipt`,
    /// `server_preparation_is_real`) are statements about an artifact or about
    /// this build rather than about the deployment, and say so in evidence.
    pub items: Vec<DeveloperEnableItem>,
}

/// `GET /api/v1/developer/readiness` — admin, read-only, advisory.
pub(crate) async fn readiness(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<DeveloperReadiness>, ApiError> {
    // Propagated, not defaulted. An empty map from a failed replicated read
    // would render as "the switch is off", which is a reading this route did
    // not take — the same dishonesty it exists to remove, one layer down.
    let settings = state.store.settings_snapshot().await?;
    let control_advertised = settings
        .get(plurx_core::store::keys::PLAYBACK_CONTROL_PROTOCOL_V1)
        .map(|value| value.trim())
        == Some("1");

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

    // "Fit the deployment" is a judgement about state size, transfer rate and
    // the container's health start period, and this process holds none of
    // those. It holds the three numbers, which are the useful half; reporting
    // them under `Met` would assert the half it cannot see.
    requirements.push(DeveloperRequirement {
        id: "recovery_budgets",
        title: "Recovery budgets fit the deployment",
        status: RequirementStatus::Unobservable,
        evidence: format!(
            "This node is configured for chunk {} s, whole transfer {} s, final install {} s, and \
             startup validation accepted them (it requires chunk \u{2264} transfer). Whether they \
             cover your measured state size, and whether the container's health start period \
             leaves room for a final install, are facts the daemon does not hold.",
            budgets.chunk_secs, budgets.transfer_secs, budgets.install_secs
        ),
    });

    requirements.push(cluster_api_requirement(state).await);
    requirements.push(cache_revocation_requirement(state).await);

    // The 20+20 campaign is a `make cluster-transport-recovery-check`
    // artifact validated by plurx-cluster-check. No running daemon has ever
    // held it, and inventing a proxy for it here would be exactly the
    // dishonesty this route replaces.
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

/// The roster half of "authenticated diagnostics are reachable".
///
/// One of the requirement's three clauses is readable and two are not. The
/// readable one is worth reading: `advertised_host` rewrites a loopback API
/// address to `localhost`, so a roster full of `localhost` is a cluster whose
/// nodes advertise addresses no peer can dial — a real and silent
/// misconfiguration. A non-loopback host proves only that a host was
/// registered: nothing here dials it, nothing here knows whether the cluster
/// API credential is shared, and nothing here knows whether the listener is
/// exposed to the public Internet. So the best case is `Unobservable` with the
/// roster reported, not `Met`.
async fn cluster_api_requirement(state: &AppState) -> DeveloperRequirement {
    const ID: &str = "cluster_api_advertised";
    const TITLE: &str = "Authenticated diagnostics are reachable";

    let status = match state.membership.status().await {
        Ok(status) => status,
        Err(MembershipError::Unavailable) => {
            return DeveloperRequirement {
                id: ID,
                title: TITLE,
                status: RequirementStatus::Unobservable,
                evidence: "This node is not replicated, so it holds no committed roster to read."
                    .to_owned(),
            };
        }
        Err(error) => {
            return DeveloperRequirement {
                id: ID,
                title: TITLE,
                status: RequirementStatus::Unobservable,
                evidence: format!(
                    "This node is replicated, but reading the committed roster failed, so the \
                     answer is unknown rather than negative ({error})."
                ),
            };
        }
    };

    let unreachable = status
        .nodes
        .iter()
        .filter(|node| {
            let host = node.advertised_host.trim();
            host.is_empty() || host.eq_ignore_ascii_case("localhost")
        })
        .map(|node| node.node_id.clone())
        .collect::<Vec<_>>();

    if unreachable.is_empty() {
        DeveloperRequirement {
            id: ID,
            title: TITLE,
            status: RequirementStatus::Unobservable,
            evidence: format!(
                "All {} committed node(s) advertise a routable cluster API host, which is the one \
                 clause of this requirement the daemon can read. It does not dial them, does not \
                 know whether every node shares the cluster API credential, and cannot see \
                 whether /cluster/transport/sqlite is reachable beyond your trusted network.",
                status.nodes.len()
            ),
        }
    } else {
        DeveloperRequirement {
            id: ID,
            title: TITLE,
            status: RequirementStatus::Unmet,
            evidence: format!(
                "{} of {} committed node(s) advertise a cluster API address no peer can dial \
                 (empty or loopback): {}.",
                unreachable.len(),
                status.nodes.len(),
                unreachable.join(", ")
            ),
        }
    }
}

/// The cache-revocation capability, and the fail-open value it must not report.
///
/// `cache_admin_revocation_ready()` answers `Ok(true)` for an unreplicated node
/// — correctly, because its caller asks "may cache-only recovery auth
/// proceed", and on a node with no cluster there is nothing to revoke. Read as
/// a prerequisite reading it would claim "every committed node proved the
/// capability in its current heartbeat" on an install that has no committed
/// nodes and takes no heartbeats. The replication check has to come first.
async fn cache_revocation_requirement(state: &AppState) -> DeveloperRequirement {
    const ID: &str = "cache_revocation_capability";
    const TITLE: &str = "Every committed node proves cache revocation";

    if !state.membership.is_replicated() {
        return DeveloperRequirement {
            id: ID,
            title: TITLE,
            status: RequirementStatus::Unobservable,
            evidence: "This node is not replicated: there is no committed configuration, so there \
                       is nothing that could have proven the capability."
                .to_owned(),
        };
    }

    match state.membership.cache_admin_revocation_ready().await {
        Ok(true) => DeveloperRequirement {
            id: ID,
            title: TITLE,
            status: RequirementStatus::Met,
            evidence: "Every member of the exact committed configuration wrote the cache \
                       revocation capability inside its current heartbeat."
                .to_owned(),
        },
        Ok(false) => DeveloperRequirement {
            id: ID,
            title: TITLE,
            status: RequirementStatus::Unmet,
            evidence: "The local applied roster does not show every committed member proving the \
                       capability in its current heartbeat, so cache-only recovery authorization \
                       stays closed. This also reads false while this node is itself outside \
                       committed membership, so check whether Cluster \u{2192} Nodes lists this \
                       node before looking for a faulty peer."
                .to_owned(),
        },
        Err(error) => DeveloperRequirement {
            id: ID,
            title: TITLE,
            status: RequirementStatus::Unobservable,
            evidence: format!("The local applied roster could not be read ({error})."),
        },
    }
}

fn playback_control_protocol(advertised: bool) -> DeveloperEnableItem {
    let vocabulary = crate::playback_control::control_vocabulary_snapshot();
    let complete = vocabulary.complete.iter().sum::<u64>();
    let partial = vocabulary.partial.iter().sum::<u64>();

    // "Every client" cannot be read upward from this counter. A client with no
    // reporter never performs a control exchange, so it appears in neither
    // column: any number of complete exchanges is consistent with a fleet full
    // of silent clients. Downward it reads fine — one partial exchange proves
    // a client in the fleet is not fully declared. So `Unmet` is reachable and
    // `Met` is not, which is the honest shape of this question.
    let reporters = if partial > 0 {
        DeveloperRequirement {
            id: "clients_report",
            title: "Every client has a passive reporter",
            status: RequirementStatus::Unmet,
            evidence: format!(
                "{partial} exchange(s) since this process started came from a client declaring \
                 only part of the action vocabulary, so at least one client in the fleet is not \
                 fully declared. By platform (web/apple/android): partial {:?}, complete {:?}.",
                vocabulary.partial, vocabulary.complete
            ),
        }
    } else if complete > 0 {
        DeveloperRequirement {
            id: "clients_report",
            title: "Every client has a passive reporter",
            status: RequirementStatus::Unobservable,
            evidence: format!(
                "{complete} exchange(s) since this process started declared the full vocabulary \
                 and none were partial \u{2014} but a client with no reporter never performs an \
                 exchange at all, so this cannot speak for clients it has not heard from. By \
                 platform (web/apple/android): complete {:?}.",
                vocabulary.complete
            ),
        }
    } else {
        DeveloperRequirement {
            id: "clients_report",
            title: "Every client has a passive reporter",
            status: RequirementStatus::Unobservable,
            evidence: "No client has completed a control exchange with this process since it \
                       started, so it has nothing to report. These counters are process-local and \
                       a restart returns them to zero."
                .to_owned(),
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
    let prepare_capable = crate::playback_control::prepare_capable_snapshot();
    let declared = prepare_capable.iter().sum::<u64>();

    let mut requirements = Vec::with_capacity(3);

    // A statement about this build, not this deployment. It is pinned from the
    // other side by `a_prepared_successor_still_publishes_a_staged_encoder` in
    // `http::hls` — when a candidate worker exists, that test fails, and
    // whoever makes it pass is holding the reason this row has to change.
    requirements.push(DeveloperRequirement {
        id: "server_preparation_is_real",
        title: "Server preparation is real",
        status: RequirementStatus::Unmet,
        evidence: "This build stages metadata only: a prepared successor publishes \
                   `encoder: staged` with no candidate worker behind its playlist, and no \
                   capacity is reserved for it."
            .to_owned(),
    });

    // Counted separately from the full-vocabulary counter on purpose. A client
    // may declare `prepare_replacement` without declaring `retry_resource`;
    // folding the two would let this row report that nobody asked for a
    // handoff while a client was asking for exactly that.
    requirements.push(if declared > 0 {
        DeveloperRequirement {
            id: "client_two_player_handoff",
            title: "A client owns a measured two-player handoff",
            status: RequirementStatus::Unmet,
            evidence: format!(
                "{declared} exchange(s) since start declared `prepare_replacement` (by platform \
                 web/apple/android: {prepare_capable:?}), but declaring the action is not the \
                 measured handoff: no shipped client aligns a second timeline with \
                 `media_origin_ms`, switches visibly, and then reports the switch. Preparations \
                 since start: {} staged, {} refused.",
                staged.staged, staged.refused
            ),
        }
    } else {
        DeveloperRequirement {
            id: "client_two_player_handoff",
            title: "A client owns a measured two-player handoff",
            status: RequirementStatus::Unmet,
            evidence: format!(
                "No client has declared `prepare_replacement` to this process since it started, \
                 and no shipped client implements the two-player switch this requirement \
                 describes. Preparations since start: {} staged, {} refused.",
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
