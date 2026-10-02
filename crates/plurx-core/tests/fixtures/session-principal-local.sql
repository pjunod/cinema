INSERT INTO users (id, username, password_hash, is_admin, created_at) VALUES (1, 'owner', 'hash', 0, 1);
         INSERT INTO media_session_requests
           (user_id, request_id, request_fingerprint, playback_id, state,
            claim_expires_at_ms, incarnation_id, owner_node_id, response_json, updated_at_ms)
         VALUES (1, 'request', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'playback', 'starting', 9000, 'live', 'node', NULL, 10);
         INSERT INTO media_sessions
           (incarnation_id, session_id, user_id, playback_id, request_fingerprint,
            owner_node_id, owner_epoch, lease_expires_at_ms, state, recipe_json,
            response_json, updated_at_ms, recovery_epoch, drain_deadline_ms, terminal_reason)
         VALUES ('live', 'live-session', 1, 'playback', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'node', 2, 9000,
                 'active', '{}', '{}', 10, 'epoch', 8000, NULL),
                ('ended', 'ended-session', 1, 'ended-playback', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'node', 1, 100,
                 'ended', '{}', '{}', 10, 'epoch', NULL, 'revoked');
         INSERT INTO media_playback_desired
           (user_id, playback_id, revision, digest, canonical_form, updated_at_ms)
         VALUES (1, 'playback', 1, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', '{}', 10);
         INSERT INTO media_playback_pointers
           (user_id, playback_id, current_incarnation_id, updated_at_ms, desired_revision)
         VALUES (1, 'playback', 'live', 10, 1);
         INSERT INTO media_session_preparations
           (user_id, playback_id, staged_incarnation_id, expected_predecessor_incarnation_id,
            deadline_ms, created_at_ms, updated_at_ms)
         VALUES (1, 'playback', 'staged', 'live', 8000, 10, 10);
         INSERT INTO media_session_producer_recovery
           (user_id, playback_id, recovery_epoch, failed_incarnation_id, failed_producer_attempt,
            decision_sequence, failed_plan_digest, alternate_plan_digest,
            decode_restriction, state, created_at_ms, updated_at_ms)
         VALUES (1, 'playback', 'epoch', 'live', 1, 1, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                 NULL, 'exhausted', 10, 11);
         INSERT INTO library_channel_session_recipes
           (user_id, request_id, incarnation_id, recipe_json, created_at_ms)
         VALUES (1, 'request', 'live', '{}', 10);
         INSERT INTO job_leases (resource, owner_node_id, fence, revision, expires_at_ms, updated_at_ms)
         VALUES ('media:live', 'node', 2, 3, 9000, 10);
         INSERT INTO media_session_terminal_acks
           (incarnation_id, session_id, owner_node_id, owner_epoch, client_instance_id,
            sequence, request_fingerprint, response_json, expires_at_ms, updated_at_ms)
         VALUES ('ended', 'ended-session', 'node', 1, 'client', 1, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', '{}', 9000, 10);
