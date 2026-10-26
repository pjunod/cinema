//! Root-scoped adaptive intent, with the existing worker wire shape.
//! Hashing an owned worker envelope must not deep-clone its application graph.
use super::*;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub(crate) fn bounded_digest<T: serde::Serialize>(value: &T) -> Result<String, String> {
    struct HashedEnvelope {
        hash: Sha256,
        bytes: usize,
    }
    impl std::io::Write for HashedEnvelope {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes = self
                .bytes
                .checked_add(bytes.len())
                .filter(|count| *count <= MAX_ADAPTIVE_ATTEMPT_REQUEST_BYTES)
                .ok_or_else(|| {
                    std::io::Error::other("adaptive binding exceeds its fixed envelope")
                })?;
            self.hash.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = HashedEnvelope {
        hash: Sha256::new(),
        bytes: 0,
    };
    serde_json::to_writer(&mut output, value)
        .map_err(|_| "adaptive source binding exceeds its bounded request envelope".to_owned())?;
    Ok(hex::encode(output.hash.finalize()))
}

/// Same field order, namespace aliases and omission rules as the owned Wire.
/// The exhaustive source destructuring below makes a new envelope field a
/// compile error until its immutable intent is explicitly covered.
#[derive(Serialize)]
struct BorrowedIntent<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    retained_output_receiver: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retained_output: Option<&'a crate::transcode::RetainedOutputFacts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    candidate_catalog: Option<&'a CandidateCatalogContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    candidate_id: Option<&'a plurx_core::playback::candidate::CandidateId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    presentation_target: Option<&'a plurx_core::playback::candidate::PresentationTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decoder_caps: Option<&'a crate::playback_control::DecoderCapsSnapshot>,
    protocol_version: i64,
    incarnation_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    principal: Option<&'a plurx_core::playback_principal::PlaybackPrincipal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<i64>,
    source_size: i64,
    source_mtime: i64,
    typeless_playlist: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    library_channel: Option<&'a serde_json::Value>,
    request: &'a SessionRequest,
}

impl RemoteStartRequest {
    fn borrowed_intent<'a>(&'a self, root_incarnation: &'a str) -> BorrowedIntent<'a> {
        let Self {
            retained_output_receiver,
            retained_output,
            candidate_catalog,
            candidate_id,
            presentation_target,
            decoder_caps,
            protocol_version,
            incarnation_id: _,
            principal,
            source_size,
            source_mtime,
            typeless_playlist,
            library_channel,
            request,
        } = self;
        let (principal, user_id) = match principal {
            plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id } => {
                (None, Some(*user_id))
            }
            principal => (Some(principal), None),
        };
        BorrowedIntent {
            retained_output_receiver: *retained_output_receiver,
            retained_output: retained_output.as_ref(),
            candidate_catalog: candidate_catalog.as_ref(),
            candidate_id: candidate_id.as_ref(),
            presentation_target: presentation_target.as_ref(),
            decoder_caps: decoder_caps.as_ref(),
            protocol_version: *protocol_version,
            incarnation_id: root_incarnation,
            principal,
            user_id,
            source_size: *source_size,
            source_mtime: *source_mtime,
            typeless_playlist: *typeless_playlist,
            library_channel: library_channel.as_ref(),
            request,
        }
    }

    /// The origin computes this once before rewriting per-attempt identity.
    pub(crate) fn adaptive_intent_digest(&self) -> Result<String, String> {
        bounded_digest(&self.borrowed_intent(&self.incarnation_id))
    }

    /// Rederive the same root-scoped wire at the worker, preserving the actual
    /// route envelope on success, bounded serialization refusal and unwind.
    /// Canonical UUID bytes replace equal-length bytes in the EXISTING request
    /// String; no temporary identity Strings or envelope clones are allocated.
    pub(crate) fn adaptive_root_intent_digest(&mut self, root: Uuid) -> Result<String, String> {
        if root.is_nil()
            || self.request.request_id.as_deref() != Some(self.incarnation_id.as_str())
            || Uuid::parse_str(&self.incarnation_id).is_err()
        {
            return Err("adaptive worker request identity is invalid".into());
        }
        let original: [u8; 36] = self
            .incarnation_id
            .as_bytes()
            .try_into()
            .map_err(|_| "adaptive worker request identity is not canonical".to_owned())?;
        let mut root_bytes = [0u8; 36];
        let root_text = root.hyphenated().encode_lower(&mut root_bytes);
        struct Restore<'a> {
            recipe: &'a mut RemoteStartRequest,
            original: [u8; 36],
        }
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                let previous = std::str::from_utf8(&self.original).expect("original UTF-8 UUID");
                self.recipe
                    .request
                    .request_id
                    .as_mut()
                    .expect("same owned request id")
                    .replace_range(.., previous);
            }
        }
        self.request
            .request_id
            .as_mut()
            .expect("validated request id")
            .replace_range(.., root_text);
        let restore = Restore {
            recipe: self,
            original,
        };
        bounded_digest(&restore.recipe.borrowed_intent(root_text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_adaptive_intent_preserves_exact_owned_wire_bytes() {
        let mut request = super::super::tests::valid_start_request();
        request.retained_output_receiver = Some(1);
        request.candidate_id =
            Some(plurx_core::playback::candidate::CandidateId::for_recipe_digest([0xa5; 32]));
        request.library_channel =
            Some(serde_json::json!({"text":"quotes\" and \\ and \n unicode λ", "id":17}));
        let owned = serde_json::to_vec(&request).expect("owned wire");
        let borrowed = serde_json::to_vec(&request.borrowed_intent(&request.incarnation_id))
            .expect("borrowed wire");
        assert_eq!(borrowed, owned);
        assert_eq!(
            request.adaptive_intent_digest().expect("borrowed digest"),
            bounded_digest(&request).expect("existing owned wire digest")
        );
    }

    #[test]
    fn adaptive_root_intent_is_attempt_invariant_and_preserves_owned_identity() {
        let mut request = super::super::tests::valid_start_request();
        let root = Uuid::parse_str(&request.incarnation_id).expect("root UUID");
        let original = request
            .adaptive_intent_digest()
            .expect("original root intent");
        for attempt in [Uuid::from_u128(0x71), Uuid::from_u128(0x72)] {
            request.incarnation_id = attempt.to_string();
            request.request.request_id = Some(request.incarnation_id.clone());
            let before = serde_json::to_vec(&request).expect("attempt wire");
            let identity = request.request.request_id.as_ref().expect("request id");
            let (address, capacity) = (identity.as_ptr(), identity.capacity());
            assert_eq!(
                request
                    .adaptive_root_intent_digest(root)
                    .expect("root intent"),
                original
            );
            assert_eq!(serde_json::to_vec(&request).expect("restored wire"), before);
            let identity = request
                .request
                .request_id
                .as_ref()
                .expect("same request id");
            assert_eq!(
                (identity.as_ptr(), identity.capacity()),
                (address, capacity)
            );
        }
        request.request.audio_index = Some(8);
        assert_ne!(
            request
                .adaptive_root_intent_digest(root)
                .expect("changed intent"),
            original
        );
        request.request.audio_index = Some(1);
        request.typeless_playlist = !request.typeless_playlist;
        assert_ne!(
            request
                .adaptive_root_intent_digest(root)
                .expect("changed playlist intent"),
            original
        );
    }

    #[test]
    fn refused_adaptive_root_hash_restores_the_same_route_and_request_allocation() {
        let mut request = super::super::tests::valid_start_request();
        let root = Uuid::parse_str(&request.incarnation_id).expect("root UUID");
        request.incarnation_id = Uuid::from_u128(0x81).to_string();
        request.request.request_id = Some(request.incarnation_id.clone());
        request.request.playback_id = "x".repeat(MAX_ADAPTIVE_ATTEMPT_REQUEST_BYTES + 1);
        let identity = request.request.request_id.as_ref().expect("request id");
        let (address, capacity) = (identity.as_ptr(), identity.capacity());
        assert!(request.adaptive_root_intent_digest(root).is_err());
        assert_eq!(
            request.request.request_id.as_deref(),
            Some(request.incarnation_id.as_str())
        );
        let identity = request
            .request
            .request_id
            .as_ref()
            .expect("restored request id");
        assert_eq!(
            (identity.as_ptr(), identity.capacity()),
            (address, capacity)
        );
    }
}
