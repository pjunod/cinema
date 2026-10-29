//! SQLite-backed tests of the public DVR intent handlers.
use super::*;
use axum::response::IntoResponse;
use plurx_core::domain::User;
use serde_json::{json, Value};

async fn fixture() -> (AppState, User, User, User) {
    let state = crate::http::tests::test_state_without_router();
    let owner = state
        .store
        .create_user("dvr-owner", "unused", false)
        .await
        .expect("owner");
    let other = state
        .store
        .create_user("dvr-other", "unused", false)
        .await
        .expect("other");
    let admin = state
        .store
        .create_user("dvr-admin", "unused", true)
        .await
        .expect("admin");
    (state, owner, other, admin)
}

fn request<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("request shape")
}

async fn refused<T>(result: Result<T, ApiError>, status: StatusCode) -> Value {
    let error = match result {
        Ok(_) => panic!("request unexpectedly succeeded"),
        Err(error) => error,
    };
    let response = error.into_response();
    assert_eq!(response.status(), status);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("error body");
    serde_json::from_slice(&bytes).expect("error JSON")
}

async fn recording(
    state: &AppState,
    user: &User,
    channel: &str,
    state_value: DvrState,
) -> DvrRecording {
    let start = now_seconds() + 3_600;
    let mut row = recording_from_programme(
        &ResolvedChannel {
            id: channel.into(),
            guide_number: channel.into(),
            name: "Fixture channel".into(),
        },
        &LiveTvProgramme {
            start,
            end: start + 1_800,
            title: "Evening programme".into(),
            episode_title: Some("Episode".into()),
            episode: None,
            synopsis: None,
            image_url: None,
            original_air_date: None,
            series_id: Some("series-7".into()),
            programme_id: None,
            is_new: Some(true),
            filters: Vec::new(),
        },
        DvrOrigin::Manual,
        None,
        Some(user.id),
        60,
        120,
        now_ms(),
    );
    row.state = state_value;
    let event = lifecycle_event(
        "scheduled",
        now_ms(),
        None,
        Some(user.id),
        None,
        json!({}),
        false,
    );
    assert_eq!(
        state
            .store
            .insert_dvr_airing_with_event(&row, &event)
            .await
            .expect("insert recording"),
        DvrInsertOutcome::Inserted
    );
    row
}

#[tokio::test]
async fn cancel_restore_and_delete_are_explicit_and_record_an_ordered_history() {
    let (state, owner, _, _) = fixture().await;
    let row = recording(&state, &owner, "7.1", DvrState::Scheduled).await;
    let Json(found) = get_recording(
        AuthUser(owner.clone()),
        State(state.clone()),
        Path(row.id.clone()),
    )
    .await
    .expect("read");
    assert_eq!(found.title, "Evening programme");
    assert_eq!(
        delete_recording(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(row.id.clone()),
            Query(DeleteRecordingQuery { delete_file: None })
        )
        .await
        .expect("cancel")
        .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        state
            .store
            .get_dvr_recording(&row.id)
            .await
            .expect("read")
            .expect("row")
            .state,
        DvrState::Cancelled
    );
    let Json(restored) = restore_recording(
        AuthUser(owner.clone()),
        State(state.clone()),
        Path(row.id.clone()),
    )
    .await
    .expect("restore");
    assert_eq!(restored.state, DvrState::Scheduled);
    refused(
        restore_recording(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(row.id.clone()),
        )
        .await,
        StatusCode::CONFLICT,
    )
    .await;
    delete_recording(
        AuthUser(owner.clone()),
        State(state.clone()),
        Path(row.id.clone()),
        Query(DeleteRecordingQuery { delete_file: None }),
    )
    .await
    .expect("cancel again");
    let error = refused(
        delete_recording(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(row.id.clone()),
            Query(DeleteRecordingQuery { delete_file: None }),
        )
        .await,
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(error["code"], "delete_file_required");
    assert_eq!(
        delete_recording(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(row.id.clone()),
            Query(DeleteRecordingQuery {
                delete_file: Some(1)
            })
        )
        .await
        .expect("explicit delete")
        .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        delete_recording(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(row.id.clone()),
            Query(DeleteRecordingQuery { delete_file: None })
        )
        .await
        .expect("idempotent delete")
        .status(),
        StatusCode::NO_CONTENT
    );
    let Json(page) = recording_events(
        AuthUser(owner.clone()),
        State(state.clone()),
        Path(row.id.clone()),
        Query(EventsQuery {
            before: None,
            after: None,
            limit: Some(100),
        }),
    )
    .await
    .expect("history");
    assert!(page.history_complete);
    assert_eq!(
        page.rows
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["deleted", "cancelled", "restored", "cancelled", "scheduled"]
    );
    assert!(page.next.is_none());
    assert!(page
        .rows
        .windows(2)
        .all(|events| events[0].sequence > events[1].sequence));
    assert!(page.rows.iter().all(|event| event.recording_id == row.id));
    let Json(visible) = list_recordings(
        AuthUser(owner.clone()),
        State(state.clone()),
        Query(request(json!({}))),
    )
    .await
    .expect("normal list");
    assert!(visible.rows.is_empty());
    let Json(deleted) = list_recordings(
        AuthUser(owner),
        State(state),
        Query(request(json!({"state":"deleted"}))),
    )
    .await
    .expect("deleted list");
    assert_eq!(deleted.rows.len(), 1);
}

#[tokio::test]
async fn running_stop_is_pending_and_repeating_it_does_not_close_or_duplicate_the_recording() {
    let (state, owner, _, _) = fixture().await;
    let row = recording(&state, &owner, "8.1", DvrState::Recording).await;
    for _ in 0..2 {
        let response = delete_recording(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(row.id.clone()),
            Query(DeleteRecordingQuery {
                delete_file: Some(1),
            }),
        )
        .await
        .expect("stop intent");
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("pending body");
        let body: Value = serde_json::from_slice(&bytes).expect("pending JSON");
        assert_eq!(body["pending"], true);
        assert_eq!(body["requested_by_user_id"], owner.id);
    }
    let stored = state
        .store
        .get_dvr_recording(&row.id)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(
        stored.state,
        DvrState::Recording,
        "only the capture owner closes the file"
    );
    assert!(stored.stop_requested_at_ms.is_some());
    let events = state
        .store
        .list_dvr_events(&row.id, None, None, 100)
        .await
        .expect("events");
    assert_eq!(
        events
            .rows
            .iter()
            .filter(|event| event.kind == "stop_requested")
            .count(),
        1
    );
}

#[tokio::test]
async fn recording_and_event_pages_are_bounded_and_reject_ambiguous_or_invalid_cursors() {
    let (state, owner, _, _) = fixture().await;
    let one = recording(&state, &owner, "1.1", DvrState::Scheduled).await;
    recording(&state, &owner, "2.1", DvrState::Scheduled).await;
    let Json(first) = list_recordings(
        AuthUser(owner.clone()),
        State(state.clone()),
        Query(request(json!({"state":"scheduled, ,recording","limit":1}))),
    )
    .await
    .expect("first page");
    assert_eq!(first.rows.len(), 1);
    let Json(second) = list_recordings(
        AuthUser(owner.clone()),
        State(state.clone()),
        Query(request(
            json!({"limit":1,"after":first.next.expect("cursor")}),
        )),
    )
    .await
    .expect("second page");
    assert_eq!(second.rows.len(), 1);
    assert_ne!(first.rows[0].id, second.rows[0].id);
    refused(
        list_recordings(
            AuthUser(owner.clone()),
            State(state.clone()),
            Query(request(json!({"state":"bogus"}))),
        )
        .await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    for query in [
        json!({"before":"1","after":"0"}),
        json!({"after":"-1"}),
        json!({"before":"not-a-number"}),
    ] {
        refused(
            recording_events(
                AuthUser(owner.clone()),
                State(state.clone()),
                Path(one.id.clone()),
                Query(request(query)),
            )
            .await,
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    let Json(events) = recording_events(
        AuthUser(owner.clone()),
        State(state.clone()),
        Path(one.id.clone()),
        Query(request(json!({"limit":1}))),
    )
    .await
    .expect("event page");
    assert_eq!(events.rows.len(), 1);
    let Json(end) = recording_events(
        AuthUser(owner.clone()),
        State(state.clone()),
        Path(one.id),
        Query(request(
            json!({"before":events.next.expect("event cursor")}),
        )),
    )
    .await
    .expect("event page end");
    assert!(end.rows.is_empty());
    for id in ["missing"] {
        refused(
            get_recording(
                AuthUser(owner.clone()),
                State(state.clone()),
                Path(id.into()),
            )
            .await,
            StatusCode::NOT_FOUND,
        )
        .await;
        refused(
            recording_events(
                AuthUser(owner.clone()),
                State(state.clone()),
                Path(id.into()),
                Query(request(json!({}))),
            )
            .await,
            StatusCode::NOT_FOUND,
        )
        .await;
        refused(
            delete_recording(
                AuthUser(owner.clone()),
                State(state.clone()),
                Path(id.into()),
                Query(request(json!({}))),
            )
            .await,
            StatusCode::NOT_FOUND,
        )
        .await;
        refused(
            restore_recording(
                AuthUser(owner.clone()),
                State(state.clone()),
                Path(id.into()),
            )
            .await,
            StatusCode::NOT_FOUND,
        )
        .await;
    }
}

#[tokio::test]
async fn recording_configuration_and_unknown_guide_requests_fail_without_persisting_intent() {
    let (state, owner, _, _) = fixture().await;
    let input = || request(json!({"channel_id":"missing","airing_start":now_seconds()+3_600}));
    let disabled = refused(
        create_recording(AuthUser(owner.clone()), State(state.clone()), Json(input())).await,
        StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;
    assert_eq!(disabled["code"], "dvr_disabled");
    state
        .store
        .put_setting(keys::DVR_ENABLED, "true")
        .await
        .expect("enable");
    let absent = refused(
        create_recording(AuthUser(owner.clone()), State(state.clone()), Json(input())).await,
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(absent["code"], "airing_unknown");
    let Json(rows) = list_recordings(
        AuthUser(owner.clone()),
        State(state.clone()),
        Query(request(json!({}))),
    )
    .await
    .expect("list");
    assert!(rows.rows.is_empty());
    let reminder = refused(
        create_reminder(
            AuthUser(owner),
            State(state.clone()),
            Json(request(
                json!({"channel_id":"missing","airing_start":now_seconds()+3_600}),
            )),
        )
        .await,
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(reminder["code"], "airing_unknown");
    assert!(state
        .store
        .list_dvr_recordings_in(&[DvrState::Scheduled])
        .await
        .expect("no writes")
        .is_empty());
}

#[tokio::test]
async fn rule_lifecycle_enforces_owner_admin_and_complete_order_without_partial_mutations() {
    let (state, owner, other, admin) = fixture().await;
    let input = || {
        request(
            json!({"name":" Evening Show ","match_mode":"title","match_value":"  EVENING  SHOW ","keep_mode":"all","new_only":false,"channel_id":"7.1","pad_start_s":-1,"pad_end_s":DVR_PAD_MAX_S+1}),
        )
    };
    refused(
        create_rule(AuthUser(owner.clone()), State(state.clone()), Json(input())).await,
        StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;
    state
        .store
        .put_setting(keys::DVR_ENABLED, "true")
        .await
        .expect("enable");
    let (status, Json(one)) =
        create_rule(AuthUser(owner.clone()), State(state.clone()), Json(input()))
            .await
            .expect("create");
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(one.name, "Evening Show");
    assert_eq!(one.match_value, normalise_title("EVENING SHOW"));
    assert_eq!(one.pad_start_s, 0);
    assert_eq!(one.pad_end_s, DVR_PAD_MAX_S);
    let (_, Json(two)) = create_rule(
        AuthUser(owner.clone()),
        State(state.clone()),
        Json(request(
            json!({"name":"Second","match_mode":"series_id","match_value":"series-7"}),
        )),
    )
    .await
    .expect("second");
    assert!(two.priority > one.priority);
    refused(
        update_rule(
            AuthUser(other.clone()),
            State(state.clone()),
            Path(one.id.clone()),
            Json(request(json!({"name":"Hijack"}))),
        )
        .await,
        StatusCode::FORBIDDEN,
    )
    .await;
    refused(
        delete_rule(AuthUser(other), State(state.clone()), Path(one.id.clone())).await,
        StatusCode::FORBIDDEN,
    )
    .await;
    for body in [
        json!({"name":""}),
        json!({"match_value":""}),
        json!({"match_mode":"wat"}),
        json!({"keep_mode":"wat"}),
        json!({"keep_mode":"last_n","keep_value":0}),
        json!({"keep_mode":"days","keep_value":0}),
    ] {
        refused(
            update_rule(
                AuthUser(owner.clone()),
                State(state.clone()),
                Path(one.id.clone()),
                Json(request(body)),
            )
            .await,
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    assert_eq!(
        state
            .store
            .get_dvr_rule(&one.id)
            .await
            .expect("read")
            .expect("rule")
            .name,
        "Evening Show"
    );
    refused(
        reorder_rules(
            AdminUser(admin.clone()),
            State(state.clone()),
            Json(RuleOrder {
                ids: vec![one.id.clone(), one.id.clone()],
            }),
        )
        .await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    let Json(ordered) = reorder_rules(
        AdminUser(admin.clone()),
        State(state.clone()),
        Json(RuleOrder {
            ids: vec![two.id.clone(), one.id.clone()],
        }),
    )
    .await
    .expect("complete reorder");
    assert_eq!(
        ordered
            .iter()
            .map(|rule| rule.id.as_str())
            .collect::<Vec<_>>(),
        vec![two.id.as_str(), one.id.as_str()]
    );
    let Json(updated) = update_rule(AuthUser(admin.clone()), State(state.clone()), Path(one.id.clone()), Json(request(json!({"name":"Administrative correction","any_channel":true,"new_only":true,"keep_mode":"days","keep_value":2,"enabled":false})))).await.expect("admin edit");
    assert!(updated.channel_id.is_none());
    assert!(!updated.enabled);
    assert_eq!(updated.keep_value, 2);
    assert_eq!(
        delete_rule(AuthUser(owner), State(state.clone()), Path(one.id.clone()))
            .await
            .expect("owner delete"),
        StatusCode::NO_CONTENT
    );
    refused(
        delete_rule(AuthUser(admin.clone()), State(state.clone()), Path(one.id)).await,
        StatusCode::NOT_FOUND,
    )
    .await;
    let Json(remaining) = list_rules(AuthUser(admin), State(state))
        .await
        .expect("rules");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, two.id);
}

#[tokio::test]
async fn reminders_are_private_and_only_a_fired_reminder_can_be_acknowledged() {
    let (state, owner, other, _) = fixture().await;
    let row = recording(&state, &owner, "9.1", DvrState::Scheduled).await;
    let reminder = DvrReminder {
        id: "reminder-a".into(),
        user_id: owner.id,
        channel_id: row.channel_id.clone(),
        guide_number: row.guide_number.clone(),
        airing_start: row.airing_start,
        airing_end: row.airing_end,
        title: row.title.clone(),
        lead_s: 60,
        state: DvrReminderState::Armed,
        fired_at_ms: None,
        acked_at_ms: None,
        created_at_ms: now_ms(),
        updated_at_ms: now_ms(),
    };
    assert!(state
        .store
        .put_dvr_reminder(&reminder)
        .await
        .expect("seed reminder"));
    let Json(visible) = list_reminders(
        AuthUser(owner.clone()),
        State(state.clone()),
        Query(RemindersQuery { due: None }),
    )
    .await
    .expect("owner reminders");
    assert_eq!(visible.len(), 1);
    assert!(visible[0].covered_by_recording);
    let Json(hidden) = list_reminders(
        AuthUser(other.clone()),
        State(state.clone()),
        Query(RemindersQuery { due: None }),
    )
    .await
    .expect("other reminders");
    assert!(hidden.is_empty());
    refused(
        ack_reminder(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(reminder.id.clone()),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;
    assert!(state
        .store
        .set_dvr_reminder_state(
            None,
            &reminder.id,
            DvrReminderState::Armed,
            DvrReminderState::Fired,
            now_ms()
        )
        .await
        .expect("fire"));
    let Json(due) = list_reminders(
        AuthUser(owner.clone()),
        State(state.clone()),
        Query(RemindersQuery { due: Some(1) }),
    )
    .await
    .expect("due");
    assert_eq!(due.len(), 1);
    refused(
        ack_reminder(
            AuthUser(other.clone()),
            State(state.clone()),
            Path(reminder.id.clone()),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;
    refused(
        delete_reminder(
            AuthUser(other),
            State(state.clone()),
            Path(reminder.id.clone()),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;
    assert_eq!(
        ack_reminder(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(reminder.id.clone())
        )
        .await
        .expect("acknowledge"),
        StatusCode::NO_CONTENT
    );
    let Json(due) = list_reminders(
        AuthUser(owner.clone()),
        State(state.clone()),
        Query(RemindersQuery { due: Some(1) }),
    )
    .await
    .expect("no pending overlay");
    assert!(due.is_empty());
    assert_eq!(
        delete_reminder(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(reminder.id.clone())
        )
        .await
        .expect("delete own"),
        StatusCode::NO_CONTENT
    );
    refused(
        delete_reminder(AuthUser(owner), State(state), Path(reminder.id)).await,
        StatusCode::NOT_FOUND,
    )
    .await;
}

#[tokio::test]
async fn guide_airing_creation_is_idempotent_and_reminders_work_with_recording_switched_off() {
    let (state, owner, _, _) = fixture().await;
    for (key, value) in [
        (keys::LIVE_TV_ENABLED, "1"),
        (keys::LIVE_TV_GUIDE_SOURCE, "hdhomerun"),
        (keys::DVR_ENABLED, "1"),
    ] {
        state.store.put_setting(key, value).await.expect("settings");
    }
    let start = now_seconds() + 3_600;
    let programme = LiveTvProgramme {
        start,
        end: start + 1_800,
        title: "Guide facts".into(),
        episode_title: Some("Pilot".into()),
        episode: Some("S01E01".into()),
        synopsis: Some("The saved synopsis".into()),
        image_url: None,
        original_air_date: None,
        series_id: Some("series-from-guide".into()),
        programme_id: Some("programme-from-guide".into()),
        is_new: Some(true),
        filters: Vec::new(),
    };
    state
        .live_tv
        .remember_relayed_guide(
            0,
            crate::live_tv::LiveTvGuide {
                source: "hdhomerun".into(),
                freshness: crate::live_tv::GuideFreshness::Fresh,
                age_seconds: 0,
                fetched_at: Some(now_seconds()),
                window: GuideWindow {
                    start: start - 3_600,
                    end: start + 7_200,
                },
                refresh_error: None,
                next_refresh_at: Some(now_seconds() + 600),
                matched_channels: 1,
                lineup_channels: 1,
                channels: vec![crate::live_tv::guide::LiveTvGuideChannel {
                    id: "guide-channel".into(),
                    guide_number: "7.1".into(),
                    affiliate: None,
                    image_url: None,
                    programmes: vec![programme],
                }],
            },
        )
        .await;
    let input = || request(json!({"channel_id":"guide-channel","airing_start":start}));
    let (status, Json(row)) =
        create_recording(AuthUser(owner.clone()), State(state.clone()), Json(input()))
            .await
            .expect("schedule guide airing");
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(row.title, "Guide facts");
    assert_eq!(row.episode_title.as_deref(), Some("Pilot"));
    assert_eq!(row.series_id.as_deref(), Some("series-from-guide"));
    let (status, Json(replay)) =
        create_recording(AuthUser(owner.clone()), State(state.clone()), Json(input()))
            .await
            .expect("same airing");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay.id, row.id);
    let (_, Json(rule)) = create_rule(
        AuthUser(owner.clone()),
        State(state.clone()),
        Json(request(
            json!({"from_airing":{"channel_id":"guide-channel","airing_start":start}}),
        )),
    )
    .await
    .expect("rule from guide");
    assert_eq!(rule.match_mode, DvrMatchMode::SeriesId);
    assert_eq!(rule.match_value, "series-from-guide");
    assert!(rule.channel_id.is_none());
    state
        .store
        .put_setting(keys::DVR_ENABLED, "0")
        .await
        .expect("switch recording off");
    let (_, Json(reminder)) = create_reminder(
        AuthUser(owner.clone()),
        State(state.clone()),
        Json(request(
            json!({"channel_id":"guide-channel","airing_start":start,"lead_s":DVR_LEAD_MAX_S+1}),
        )),
    )
    .await
    .expect("reminder needs no tuner or recording switch");
    assert_eq!(reminder.lead_s, DVR_LEAD_MAX_S);
    let (_, Json(updated)) = create_reminder(
        AuthUser(owner.clone()),
        State(state.clone()),
        Json(request(
            json!({"channel_id":"guide-channel","airing_start":start,"lead_s":-1}),
        )),
    )
    .await
    .expect("same reminder updates");
    assert_eq!(updated.id, reminder.id);
    assert_eq!(updated.lead_s, 0);
    let Json(reminders) = list_reminders(
        AuthUser(owner),
        State(state),
        Query(RemindersQuery { due: None }),
    )
    .await
    .expect("one reminder");
    assert_eq!(reminders.len(), 1);
    assert!(reminders[0].covered_by_recording);
}

#[tokio::test]
async fn schedule_pages_filter_channels_and_keep_cancellation_visible_in_an_explicit_window() {
    let (state, owner, _, _) = fixture().await;
    let first = recording(&state, &owner, "schedule-a", DvrState::Scheduled).await;
    let second = recording(&state, &owner, "schedule-b", DvrState::Cancelled).await;
    let from = first.capture_start - 100;
    let to = first.capture_end + 100;
    let query = |extra: Vec<(String, String)>| {
        let mut q = vec![
            ("from".into(), from.to_string()),
            ("to".into(), to.to_string()),
            ("limit".into(), "1".into()),
        ];
        q.extend(extra);
        Query(q)
    };
    let Json(page) = schedule(
        AuthUser(owner.clone()),
        State(state.clone()),
        query(Vec::new()),
    )
    .await
    .expect("schedule page");
    assert_eq!(page.rows.len(), 1);
    let Json(next) = schedule(
        AuthUser(owner.clone()),
        State(state.clone()),
        query(vec![("after".into(), page.next.expect("next cursor"))]),
    )
    .await
    .expect("schedule next page");
    assert_eq!(next.rows.len(), 1);
    assert_ne!(page.rows[0].id, next.rows[0].id);
    let Json(filtered) = schedule(
        AuthUser(owner.clone()),
        State(state.clone()),
        query(vec![("channel_id".into(), second.channel_id)]),
    )
    .await
    .expect("channel filter");
    assert_eq!(filtered.rows.len(), 1);
    assert_eq!(filtered.rows[0].state, DvrState::Cancelled);
    for bad in [
        vec![("days".into(), "nonsense".into())],
        vec![("from".into(), "10".into()), ("to".into(), "9".into())],
        vec![("after".into(), "missing-colon".into())],
        vec![("after".into(), "10:".into())],
        vec![("after".into(), "bad:id".into())],
    ] {
        refused(
            schedule(AuthUser(owner.clone()), State(state.clone()), Query(bad)).await,
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    let too_many = (0..65)
        .map(|i| ("channel_id".into(), format!("channel-{i}")))
        .collect();
    refused(
        schedule(AuthUser(owner), State(state), Query(too_many)).await,
        StatusCode::BAD_REQUEST,
    )
    .await;
}

#[tokio::test]
async fn attention_paging_prioritizes_current_conditions_and_acknowledgment_is_per_viewer() {
    let (state, owner, other, _) = fixture().await;
    let current = recording(&state, &owner, "attention-current", DvrState::Conflict).await;
    let failed = recording(&state, &owner, "attention-history", DvrState::Scheduled).await;
    let event = lifecycle_event(
        "capture_failed",
        now_ms(),
        None,
        Some(owner.id),
        Some("source_unavailable"),
        json!({"attempt":1}),
        true,
    );
    assert!(state
        .store
        .transition_dvr_recording_with_event(
            &DvrTransition {
                id: &failed.id,
                from: &[DvrState::Scheduled],
                to: DvrState::Failed,
                reason: Some("source unavailable"),
                patch: DvrStatePatch::None,
                fence_generation: None,
                now_ms: now_ms()
            },
            &event
        )
        .await
        .expect("capture failure"));
    let Json(first) = attention(
        AuthUser(owner.clone()),
        State(state.clone()),
        Query(AttentionQuery {
            after: None,
            limit: Some(1),
        }),
    )
    .await
    .expect("current conditions first");
    assert_eq!(first.total, 2);
    assert_eq!(first.rows.len(), 1);
    assert_eq!(first.rows[0].recording.0.id, current.id);
    let Json(second) = attention(
        AuthUser(owner.clone()),
        State(state.clone()),
        Query(AttentionQuery {
            after: first.next,
            limit: Some(1),
        }),
    )
    .await
    .expect("historical page");
    assert_eq!(second.rows.len(), 1);
    assert_eq!(second.rows[0].recording.0.id, failed.id);
    let public = serde_json::to_value(&second.rows[0]).expect("public attention JSON");
    for key in [
        "requested_by_user_id",
        "tuner_owner_node_id",
        "path",
        "stop_requested_by_user_id",
        "stopped_by_user_id",
    ] {
        assert!(
            public["recording"].get(key).is_none(),
            "attention must omit private field {key}"
        );
    }
    assert_eq!(public["recording"]["stopped_early"], false);
    let sequence = second.rows[0].latest_attention_sequence;
    let Json(ack) = acknowledge_attention(
        AuthUser(owner.clone()),
        State(state.clone()),
        Path(failed.id.clone()),
        Json(AttentionAckRequest {
            through_sequence: sequence,
        }),
    )
    .await
    .expect("acknowledge seen failure");
    assert_eq!(ack.through_sequence, sequence);
    let Json(remaining) = attention(
        AuthUser(owner.clone()),
        State(state.clone()),
        Query(request(json!({}))),
    )
    .await
    .expect("own attention after ack");
    assert_eq!(remaining.total, 1);
    assert_eq!(remaining.rows[0].recording.0.id, current.id);
    let Json(other_viewer) = attention(
        AuthUser(other),
        State(state.clone()),
        Query(request(json!({}))),
    )
    .await
    .expect("another viewer still sees the failure");
    assert_eq!(other_viewer.total, 2);
    refused(
        acknowledge_attention(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(failed.id),
            Json(AttentionAckRequest {
                through_sequence: sequence + 100,
            }),
        )
        .await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    refused(
        attention(
            AuthUser(owner),
            State(state),
            Query(AttentionQuery {
                after: Some("not-hex".into()),
                limit: None,
            }),
        )
        .await,
        StatusCode::BAD_REQUEST,
    )
    .await;
}

#[tokio::test]
async fn overview_separates_unobserved_captures_from_future_schedule_and_hides_admin_diagnostics() {
    let (state, owner, _, admin) = fixture().await;
    let running = recording(&state, &owner, "overview-running", DvrState::Recording).await;
    let future = recording(&state, &owner, "overview-future", DvrState::Scheduled).await;
    let Json(viewer) = overview(AuthUser(owner.clone()), State(state.clone()))
        .await
        .expect("viewer overview");
    assert_eq!(viewer.active_total, Some(1));
    assert_eq!(viewer.next_capture_start, Some(future.capture_start));
    assert_eq!(viewer.active[0].recording_id, running.id);
    assert_eq!(viewer.active[0].display_state, "Status unavailable");
    assert_eq!(viewer.counts.unconfirmed, Some(1));
    assert!(viewer.active[0].can_stop);
    assert!(!viewer.active[0].can_view_diagnostics);
    assert!(!viewer.active[0].can_reorder_rules);
    assert!(viewer.diagnostics.is_none());
    assert!(viewer.active[0].last_confirmed_bytes.is_none());
    let Json(administrative) = overview(AuthUser(admin), State(state.clone()))
        .await
        .expect("admin overview");
    assert!(administrative.diagnostics.is_some());
    assert!(administrative.active[0].can_view_diagnostics);
    assert!(administrative.active[0].can_reorder_rules);
    state
        .store
        .put_setting(keys::DVR_ENABLED, "1")
        .await
        .expect("recordings on");
    state
        .store
        .put_setting(keys::DVR_FREE_FLOOR_GB, "-1")
        .await
        .expect("floor");
    state
        .store
        .put_setting(keys::DVR_TUNER_RESERVE, "99")
        .await
        .expect("reserve");
    let Json(status) = status(AuthUser(owner), State(state))
        .await
        .expect("DVR status");
    assert!(status.enabled);
    assert_eq!(status.next_start, Some(future.capture_start));
    assert_eq!(status.floor_bytes, 0);
    assert_eq!(status.slots.reserve, 4);
    assert_eq!(
        status.slots.recording, 0,
        "durable recording state alone is not an active tuner lease"
    );
}
