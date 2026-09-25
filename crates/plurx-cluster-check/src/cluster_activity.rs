//! Cross-process subtitle publication lookup on the replicated authority.
//!
//! The cluster harness runs embedded stores, not plurxd's HTTP media server.
//! This scenario proves the store boundary used by peer hydration: node A's
//! acknowledged publication is visible when node B looks up the same source
//! stamp and track. The HTTP fetch and local inode binding are exercised by
//! plurxd's subtitle-source tests.

use anyhow::{bail, Context, Result};
use plurx_core::store::SubtitleSourcePublication;
use sha2::{Digest, Sha256};

use super::{
    harness_executable, start_cluster_with_port_retry, ClusterProcesses, Request, Response,
};

pub(super) async fn subtitle_source_node_b_looks_up_node_a_publication() -> Result<()> {
    let executable = harness_executable()?;
    let root = tempfile::tempdir().context("subtitle publication cluster data root")?;
    let (mut cluster, _) = start_cluster_with_port_retry(&executable, root.path(), 3).await?;
    cluster.request(1, Request::Bootstrap).await?.require_ok()?;
    for node_id in 2..=3 {
        cluster
            .request(node_id, Request::Open)
            .await?
            .require_ok()?;
    }
    cluster.wait_for_voters(&[1, 2, 3]).await?;

    let artifact = b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\ncluster subtitle\n";
    let publication = SubtitleSourcePublication {
        file_id: 97,
        source_size: 8192,
        source_mtime: 1_700_000_000,
        // A sampled source digest is opaque to the replicated store. Its
        // portability and local-file validation are checked by plurxd.
        source_attestation: hex::encode(Sha256::digest(b"source-attestation-fixture")),
        node_id: "node-1".to_owned(),
        ordinal: 1,
        kind: "text".to_owned(),
        format: "webvtt".to_owned(),
        verdict: "kept".to_owned(),
        attempts: 1,
        origin: "extracted".to_owned(),
        sha256: hex::encode(Sha256::digest(artifact)),
        bytes: i64::try_from(artifact.len())?,
        published_at_ms: 1_700_000_001_000,
    };
    cluster
        .request(
            1,
            Request::PublishSubtitleSource {
                publication: publication.clone(),
            },
        )
        .await?
        .require_ok()?;

    let rows = lookup(&mut cluster, 2, &publication).await?;
    if rows != [publication.clone()] {
        bail!("node B did not look up node A's acknowledged subtitle track: {rows:?}");
    }
    let wrong_stamp = cluster
        .request(
            2,
            Request::LookupSubtitleSource {
                file_id: publication.file_id,
                source_size: publication.source_size + 1,
                source_mtime: publication.source_mtime,
            },
        )
        .await?;
    match wrong_stamp {
        Response::SubtitleSourcePublications { rows } if rows.is_empty() => {}
        response => bail!("node B accepted a publication for another source stamp: {response:?}"),
    }
    Ok(())
}

async fn lookup(
    cluster: &mut ClusterProcesses,
    node_id: u64,
    publication: &SubtitleSourcePublication,
) -> Result<Vec<SubtitleSourcePublication>> {
    match cluster
        .request(
            node_id,
            Request::LookupSubtitleSource {
                file_id: publication.file_id,
                source_size: publication.source_size,
                source_mtime: publication.source_mtime,
            },
        )
        .await?
    {
        Response::SubtitleSourcePublications { rows } => Ok(rows),
        response => bail!("subtitle publication lookup returned {response:?}"),
    }
}
