//! Startup-only Source activation; no handler or serving heartbeat installs DDL.
use super::*;
use crate::store::sharing_source_schema as layout;

struct PayloadRow(String);
impl From<&mut Row<'_>> for PayloadRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("payload"))
    }
}
async fn predicate(
    client: &Client,
    guard: String,
    bindings: Vec<Param>,
) -> Result<bool, MembershipError> {
    let rows = client
        .query_consistent_map::<CountRow, _>(
            format!("SELECT CASE WHEN {guard} THEN 1 ELSE 0 END AS count"),
            bindings,
        )
        .await?;
    Ok(matches!(rows.as_slice(),[row] if row.count==1))
}
async fn transaction(
    client: &Client,
    statements: Vec<(String, Vec<Param>)>,
) -> Result<(), MembershipError> {
    let results = client.txn(statements).await?;
    results.into_iter().collect::<Result<Vec<_>, _>>()?;
    Ok(())
}
async fn verify_layout(client: &Client) -> Result<bool, MembershipError> {
    let rows = client
        .query_consistent_map::<CountRow, _>(
            "SELECT schema_version AS count FROM cluster_meta WHERE singleton=1",
            params!(),
        )
        .await?;
    match rows.as_slice() {
        [row] if row.count == layout::SOURCE_SCHEMA_VERSION => {
            if predicate(client, layout::installed_guard(), params!()).await? {
                Ok(true)
            } else {
                Err(MembershipError::Incompatible)
            }
        }
        [row] if row.count == layout::SOURCE_SCHEMA_PREDECESSOR => {
            if predicate(client, layout::predecessor_layout_guard(), params!()).await? {
                boot_table_present(client).await?;
                Ok(false)
            } else {
                Err(MembershipError::Incompatible)
            }
        }
        _ => Err(MembershipError::Incompatible),
    }
}
async fn boot_table_present(client: &Client) -> Result<bool, MembershipError> {
    let rows = client
        .query_consistent_map::<CountRow, _>(
            "SELECT count(*) AS count FROM sqlite_master WHERE name='sharing_source_boot_intents'",
            params!(),
        )
        .await?;
    match rows.as_slice() {
        [row] if row.count == 0 => Ok(false),
        [row]
            if row.count == 1
                && predicate(client, layout::boot_shape_guard(), params!()).await? =>
        {
            Ok(true)
        }
        _ => Err(MembershipError::Incompatible),
    }
}
async fn ensure_boot_table(
    inner: &ReplicatedMembership,
    master: &crate::secrets::CredentialKey,
) -> Result<bool, MembershipError> {
    if boot_table_present(&inner.client).await? {
        return Ok(true);
    }
    let Some(members) =
        purpose_key_member_observation(&inner.client, inner.identity.raft_id, master).await?
    else {
        return Ok(false);
    };
    let (guard, roster, cutoff, now) = members.write_guard(unix_ms()?, 1, 2, 3)?;
    let attempt=transaction(&inner.client,vec![
        ("DELETE FROM sharing_purpose_transaction_guard".to_owned(),params!()),
        (format!("INSERT INTO sharing_purpose_transaction_guard VALUES(1,CASE WHEN ({guard}) AND NOT EXISTS(SELECT 1 FROM sharing_purpose_census_intents) AND NOT EXISTS(SELECT 1 FROM sqlite_master WHERE name='sharing_source_boot_intents') THEN 1 ELSE 0 END)"),params!(roster,cutoff,now)),
        (layout::BOOT_INTENTS_SCHEMA.to_owned(),params!()),
        ("DELETE FROM sharing_purpose_transaction_guard".to_owned(),params!()),
        (format!("INSERT INTO sharing_purpose_transaction_guard VALUES(1,CASE WHEN ({}) THEN 1 ELSE 0 END)",layout::boot_shape_guard()),params!()),
    ]).await;
    match attempt {
        Ok(()) => Ok(true),
        Err(error) => {
            if boot_table_present(&inner.client).await? {
                Ok(true)
            } else {
                Err(error)
            }
        }
    }
}
async fn install(inner: &ReplicatedMembership) -> Result<bool, MembershipError> {
    if verify_layout(&inner.client).await? {
        return Ok(true);
    }
    if !inner
        .client
        .metrics_db()
        .await?
        .membership_config
        .voter_ids()
        .any(|id| id == inner.identity.raft_id)
    {
        return Ok(false);
    }
    let master = inner
        .purpose_master
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .ok_or(MembershipError::Incompatible)?;
    let Some(members) =
        source_member_observation(&inner.client, inner.identity.raft_id, Some(&master)).await?
    else {
        return Ok(false);
    };
    let rows=inner.client.query_consistent_map::<PayloadRow,_>("SELECT json_array(intent.node_id,intent.raft_id,intent.attempt_id,intent.master_fingerprint,intent.membership_generation) AS payload FROM sharing_source_boot_intents intent JOIN cluster_nodes node ON node.node_id=intent.node_id AND node.raft_id=intent.raft_id WHERE node.removed_at IS NULL ORDER BY intent.node_id LIMIT 257",params!()).await?;
    if rows.is_empty() || rows.len() > 256 {
        return Ok(false);
    }
    let captured = serde_json::to_string(
        &rows
            .into_iter()
            .map(|row| serde_json::from_str::<serde_json::Value>(&row.0))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| MembershipError::Incompatible)?,
    )
    .map_err(|_| MembershipError::Incompatible)?;
    let (floor, roster, cutoff, now) = members.write_guard(unix_ms()?, 1, 2, 3)?;
    let boot = layout::boot_authority_guard(4);
    let closed_admission = sharing_admission_schema_shape_predicate();
    let ingress_floor = sharing_member_guard_predicate(SharingMemberFloor::IngressCustody, 1, 2, 3);
    let authority=format!("({closed_admission}) AND ({floor}) AND ({ingress_floor}) AND ({boot}) AND EXISTS(SELECT 1 FROM settings WHERE key='sharing_enabled' AND CASE WHEN typeof(value)='text' AND length(CAST(value AS BLOB))<=64 THEN lower(trim(value)) IN('1','true','yes','on') ELSE 0 END) AND NOT EXISTS(SELECT 1 FROM sharing_source_boot_intents intent JOIN cluster_nodes node ON node.node_id=intent.node_id AND node.raft_id=intent.raft_id WHERE node.removed_at IS NULL AND intent.master_fingerprint!=\'{}\') AND NOT EXISTS(SELECT 1 FROM sharing_purpose_census_intents) AND EXISTS(SELECT 1 FROM sharing_purpose_key_installation ready JOIN sharing_identity identity ON identity.singleton=ready.singleton AND identity.server_id=ready.server_id AND identity.catalogue_epoch=ready.catalogue_epoch WHERE ready.singleton=1 AND ready.state='ready') AND EXISTS(SELECT 1 FROM cluster_nodes WHERE raft_id={} AND role IS NOT 'learner' AND removed_at IS NULL)",master.sharing_purpose_master_fingerprint(),inner.identity.raft_id);
    if !predicate(
        &inner.client,
        format!("({authority}) AND ({})", layout::predecessor_guard(3)),
        params!(roster.clone(), cutoff, now, captured.clone()),
    )
    .await?
    {
        return Ok(false);
    }
    let mut statements=vec![("DELETE FROM sharing_purpose_transaction_guard".to_owned(),params!()),(format!("INSERT INTO sharing_purpose_transaction_guard VALUES(1,CASE WHEN ({authority}) AND ({}) THEN 1 ELSE 0 END)",layout::predecessor_guard(3)),params!(roster.clone(),cutoff,now,captured.clone()))];
    statements.extend(
        layout::layout_statements()
            .into_iter()
            .map(|statement| (statement, params!())),
    );
    statements.extend([
        (layout::INSTALLATION_SCHEMA.to_owned(),params!()),
        (format!("INSERT INTO sharing_source_schema_installation VALUES(1,{},$1,$2)", layout::SOURCE_LAYOUT_VERSION),params!(master.sharing_purpose_master_fingerprint(),now)),
        (layout::TRANSACTION_SCHEMA.to_owned(),params!()),
        (format!("UPDATE cluster_meta SET schema_version={},migrated_at=$1 WHERE singleton=1 AND schema_version={}", layout::SOURCE_SCHEMA_VERSION, layout::SOURCE_SCHEMA_PREDECESSOR),params!(now / 1000)),
        (format!("INSERT INTO sharing_source_schema_transaction_guard VALUES(1,CASE WHEN ({authority}) AND ({}) THEN 1 ELSE 0 END)",layout::installed_shape_guard()),params!(roster,cutoff,now,captured)),

    ]);
    let attempt = transaction(&inner.client, statements).await;
    match attempt {
        Ok(()) => verify_layout(&inner.client).await,
        Err(error) => {
            if verify_layout(&inner.client).await? {
                Ok(true)
            } else {
                Err(error)
            }
        }
    }
}
/// A previous crashed process cannot leave an installer receipt behind when
/// this boot proceeds to Local serving. The selected-master heartbeat has
/// already withdrawn its boot label; retirement asserts that absence again in
/// the same write and confirms the exact admitted node/raft has no intent.
async fn retire_previous_boot(
    inner: &ReplicatedMembership,
    master: &crate::secrets::CredentialKey,
) -> Result<(), MembershipError> {
    if !boot_table_present(&inner.client).await? {
        return Ok(());
    }
    let own = params!(
        inner.identity.node_id.as_str(),
        inner.identity.raft_id as i64
    );
    if !predicate(
        &inner.client,
        "EXISTS(SELECT 1 FROM sharing_source_boot_intents WHERE node_id=$1 AND raft_id=$2)"
            .to_owned(),
        own.clone(),
    )
    .await?
    {
        return Ok(());
    }
    let selected = format!(
        "sharing_purpose_master_v1:{}",
        master.sharing_purpose_master_fingerprint()
    );
    let safe=format!("({}) AND NOT EXISTS(SELECT 1 FROM sharing_purpose_census_intents) AND EXISTS(SELECT 1 FROM cluster_nodes node JOIN cluster_node_capabilities proof ON proof.node_id=node.node_id AND proof.last_seen_at=node.last_seen_at WHERE node.node_id=$1 AND node.raft_id=$2 AND node.removed_at IS NULL AND proof.capability=$3 AND NOT EXISTS(SELECT 1 FROM cluster_node_capabilities boot WHERE boot.node_id=node.node_id AND boot.last_seen_at=node.last_seen_at AND boot.capability GLOB 'sharing_source_boot_v1:*'))",layout::boot_shape_guard());
    transaction(&inner.client,vec![
        ("DELETE FROM sharing_purpose_transaction_guard".to_owned(),params!()),
        (format!("INSERT INTO sharing_purpose_transaction_guard VALUES(1,CASE WHEN {safe} THEN 1 ELSE 0 END)"),params!(inner.identity.node_id.as_str(),inner.identity.raft_id as i64,selected)),
        ("DELETE FROM sharing_source_boot_intents WHERE node_id=$1 AND raft_id=$2".to_owned(),own.clone()),
        ("DELETE FROM sharing_purpose_transaction_guard".to_owned(),params!()),
        ("INSERT INTO sharing_purpose_transaction_guard VALUES(1,CASE WHEN NOT EXISTS(SELECT 1 FROM sharing_source_boot_intents WHERE node_id=$1 AND raft_id=$2) THEN 1 ELSE 0 END)".to_owned(),own),
    ]).await
}

/// The older complete guard protocol remains a recognized Local-only state.
/// It grants no factory authority: only exact untouched legacy definitions,
/// generation shape and absence of new guard fragments receive this disposition.
async fn recognized_legacy_admission_schema(client: &Client) -> Result<bool, MembershipError> {
    let legacy = sharing_member_admission_guard_schema()
        .into_iter()
        .filter(|sql| !sql.contains("cluster_sharing_purpose_keys_"))
        .map(|sql| sql.replace(",'sharing_purpose_keys_v1'", ""))
        .filter(|sql| sql.starts_with("CREATE "))
        .collect::<Vec<_>>();
    let mut predicates = Vec::new();
    for sql in &legacy {
        let canonical = sql.replace(" IF NOT EXISTS", "");
        let words = canonical.split_whitespace().collect::<Vec<_>>();
        let name = words[2].split('(').next().expect("closed guard name");
        predicates.push(format!(
            "EXISTS(SELECT 1 FROM sqlite_master WHERE type='{}' AND name='{name}' AND sql='{}')",
            words[1].to_ascii_lowercase(),
            canonical.replace("'", "''")
        ));
    }
    let guard = format!(
        "(SELECT count(*) FROM sqlite_master WHERE name GLOB 'cluster_sharing_*')={} AND ({})",
        legacy.len(),
        predicates.join(") AND (")
    );
    if !predicate(client, guard, params!()).await? {
        return Ok(false);
    }
    predicate(client,format!("({}) AND NOT EXISTS(SELECT 1 FROM sqlite_master WHERE name IN('sharing_source_schema_installation','sharing_source_schema_transaction_guard'))",sharing_membership_generation_shape_predicate()),params!()).await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StartupAdmissionLayout {
    Absent,
    Legacy,
    Current,
}
async fn startup_admission_layout(
    client: &Client,
) -> Result<StartupAdmissionLayout, MembershipError> {
    let count = client
        .query_consistent_map::<CountRow, _>(
            "SELECT count(*) AS count FROM sqlite_master WHERE name GLOB 'cluster_sharing_*'",
            params!(),
        )
        .await?;
    if matches!(count.as_slice(),[row] if row.count==0) {
        return Ok(StartupAdmissionLayout::Absent);
    }
    if recognized_legacy_admission_schema(client).await? {
        return Ok(StartupAdmissionLayout::Legacy);
    }
    let expected = sharing_admission_schema_names().len();
    let bootstrap=client.query_consistent_map::<CountRow,_>("SELECT count(*) AS count FROM sqlite_master WHERE name='cluster_sharing_purpose_bootstrap_guard'",params!()).await?;
    let extra = match bootstrap.as_slice() {
        [row] if row.count == 0 => 0,
        [row] if row.count == 1 => 1,
        _ => return Err(MembershipError::Incompatible),
    };
    if !matches!(count.as_slice(),[row] if row.count==(expected+extra) as i64)
        || !predicate(
            client,
            format!(
                "({}) AND ({})",
                sharing_admission_schema_shape_predicate(),
                sharing_membership_generation_shape_predicate()
            ),
            params!(),
        )
        .await?
    {
        return Err(MembershipError::Incompatible);
    }
    if extra==1 && !predicate(client,format!("EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='cluster_sharing_purpose_bootstrap_guard' AND sql='{}') AND NOT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND tbl_name='cluster_sharing_purpose_bootstrap_guard') AND EXISTS(SELECT 1 FROM cluster_sharing_purpose_bootstrap_guard WHERE singleton=1 AND passed=1)",PURPOSE_BOOTSTRAP_GUARD_SCHEMA.replace("'","''")),params!()).await? { return Err(MembershipError::Incompatible); }
    Ok(StartupAdmissionLayout::Current)
}

impl MembershipManager {
    /// Called once by SelectedStore before constructing State or probing workers.
    /// Timeout is pending readiness only after exact confirmed intent release.
    pub(crate) async fn coordinate_source_schema_startup(&self) -> Result<bool, MembershipError> {
        let Some(inner) = &self.inner else {
            return Ok(false);
        };
        let ready = verify_layout(&inner.client).await?;
        let admission = startup_admission_layout(&inner.client).await?;
        if (ready || boot_table_present(&inner.client).await?)
            && admission != StartupAdmissionLayout::Current
        {
            return Err(MembershipError::Incompatible);
        }
        let master = inner
            .purpose_master
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or(MembershipError::Incompatible)?;
        retire_previous_boot(inner, &master).await?;
        if ready {
            return Ok(true);
        }
        let saved = inner
            .store
            .get_setting("sharing_enabled")
            .await
            .map_err(|_| MembershipError::Incompatible)?;
        // The public settings API persists booleans as "1"/"0". Use the same
        // decoder as serving/settings; an unknown or missing choice stays off.
        let enabled = crate::store::stored_switch(saved.as_deref(), false);
        if !enabled || admission == StartupAdmissionLayout::Legacy {
            // verify_layout has proved complete legacy session/catalogue shape;
            // actual SelectedStore startup has already AEAD-opened its census.
            // retire_previous_boot confirmed this node/raft has no old attempt.
            return Ok(false);
        }
        let attempt = uuid::Uuid::new_v4().to_string();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let mut registered = false;
        loop {
            self.coordinate_purpose_keys().await?;
            if ensure_boot_table(inner, &master).await? {
                let generation = sharing_membership_generation_shape_predicate();
                let fingerprint = master.sharing_purpose_master_fingerprint();
                let own_master = format!("sharing_purpose_master_v1:{fingerprint}");
                transaction(&inner.client,vec![(format!("INSERT INTO sharing_source_boot_intents SELECT $1,$2,$3,$4,generation FROM cluster_sharing_membership_generation WHERE singleton=1 AND ({generation}) AND ({}) AND NOT EXISTS(SELECT 1 FROM sharing_purpose_census_intents) AND EXISTS(SELECT 1 FROM cluster_nodes node JOIN cluster_node_capabilities proof ON proof.node_id=node.node_id AND proof.last_seen_at=node.last_seen_at WHERE node.node_id=$1 AND node.raft_id=$2 AND node.removed_at IS NULL AND proof.capability=$5) ON CONFLICT(node_id) DO UPDATE SET raft_id=excluded.raft_id,attempt_id=excluded.attempt_id,master_fingerprint=excluded.master_fingerprint,membership_generation=excluded.membership_generation",layout::boot_shape_guard()),params!(inner.identity.node_id.as_str(),inner.identity.raft_id as i64,attempt.clone(),fingerprint,own_master))]).await?;
                let own = predicate(&inner.client,"EXISTS(SELECT 1 FROM sharing_source_boot_intents WHERE node_id=$1 AND raft_id=$2 AND attempt_id=$3 AND master_fingerprint=$4 AND membership_generation=(SELECT generation FROM cluster_sharing_membership_generation WHERE singleton=1))".to_owned(),params!(inner.identity.node_id.as_str(),inner.identity.raft_id as i64,attempt.clone(),master.sharing_purpose_master_fingerprint())).await?;
                if !own {
                    return Err(MembershipError::Incompatible);
                }
                registered = true;
                *inner
                    .source_boot_attempt
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(attempt.clone());
                let mut committed = inner.heartbeat_writes.last_committed.lock().await;
                self.commit_heartbeat(inner).await?;
                *committed = Some(tokio::time::Instant::now());
                drop(committed);
                if install(inner).await? {
                    break;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        *inner
            .source_boot_attempt
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        if registered {
            transaction(&inner.client,vec![
                ("DELETE FROM sharing_purpose_transaction_guard".to_owned(),params!()),
                (format!("INSERT INTO sharing_purpose_transaction_guard VALUES(1,CASE WHEN ({}) THEN 1 ELSE 0 END)",layout::boot_shape_guard()),params!()),
                ("DELETE FROM sharing_source_boot_intents WHERE node_id=$1 AND raft_id=$2 AND attempt_id=$3".to_owned(),params!(inner.identity.node_id.as_str(),inner.identity.raft_id as i64,attempt)),
                ("DELETE FROM sharing_purpose_transaction_guard".to_owned(),params!()),
                ("INSERT INTO sharing_purpose_transaction_guard VALUES(1,CASE WHEN NOT EXISTS(SELECT 1 FROM sharing_source_boot_intents WHERE node_id=$1 AND raft_id=$2) THEN 1 ELSE 0 END)".to_owned(),params!(inner.identity.node_id.as_str(),inner.identity.raft_id as i64)),
            ]).await?;
        }
        verify_layout(&inner.client).await
    }
}
