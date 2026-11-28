//! Server-owned experimental profile, not a claim of general language quality.
use crate::subtitle_models::artifacts::CatalogModel;
use plurx_core::store::subtitle_translation::{SpeechTask, TranslationCue, TranslationProfile};

pub(crate) const MODEL_SHA256: &str =
    "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b";
pub(crate) const SELF_TEST_SHA256: &str =
    "1942763aa0b6903ba0305fe95d665497efdd75aacbb605f79d6484ac6470b33d";
pub(crate) const SELF_TEST_LANGUAGE: &str = "de";
pub(crate) const SELF_TEST_SAMPLES: i64 = 207_360;
pub(crate) const SELF_TEST_FIXTURE: &[u8] =
    include_bytes!("../../../../tests/fixtures/subtitle-translation/audio/fleurs-de-1586.wav");
pub(crate) const SELF_TEST_ATTRIBUTION: &str =
    "Google FLEURS, de_de development sentence1586, original unmodified recording. \
     Dataset revision70bb2e84b976b7e960aa89f1c648e09c59f894dd; \
     member dev/10031412691154736538.wav. \
     Source: https://huggingface.co/datasets/google/fleurs/tree/70bb2e84b976b7e960aa89f1c648e09c59f894dd. \
     CC BY4.0: https://creativecommons.org/licenses/by/4.0/. \
     Extracted only; decoded/resampled privately during the production self-test. \
     Full license: licenses/CC-BY-4.0.txt. \
     English references are publisher-provided n-way FLEURS/FLoRes sentence pairs, not agent-generated.";

const VAD_REVISION: &str = "9ffd54a1e1ee413ddf265af9913beaf518d1639b";

pub(crate) fn vad_spec() -> CatalogModel {
    CatalogModel {
        id: "silero-v5.1.2".into(),
        label: "Silero speech detection".into(),
        languages: vec![],
        download_bytes: 885_098,
        sha256: "29940d98d42b91fbd05ce489f3ecf7c72f0a42f027e4875919a28fb4c04ea2cf".into(),
        url: format!("https://huggingface.co/ggml-org/whisper-vad/resolve/{VAD_REVISION}/ggml-silero-v5.1.2.bin"),
        source: format!("https://huggingface.co/ggml-org/whisper-vad/tree/{VAD_REVISION}"),
        license: "MIT".into(),
    }
}

pub(crate) fn uses_vad(profile: &TranslationProfile) -> bool {
    matches!(
        profile.pipeline_revision.as_str(),
        "translation-window-v2" | "translation-window-v2-fast"
    )
}
pub(crate) fn fast_decode(profile: &TranslationProfile) -> bool {
    profile.pipeline_revision == "translation-window-v2-fast"
}

pub(crate) fn self_test_notice() -> String {
    format!(
        "{SELF_TEST_ATTRIBUTION}\n\n{}",
        include_str!("../../../../licenses/CC-BY-4.0.txt")
    )
}

pub(crate) fn selected_profile() -> TranslationProfile {
    TranslationProfile {
        task: SpeechTask::TranslateToEnglish,
        version: 1,
        model_id: "small".into(),
        model_sha256: MODEL_SHA256.into(),
        adapter_contract: "whisper.cpp-1.9.2-cpu-v1".into(),
        pipeline_revision: "translation-window-v1".into(),
        core_ms: 30_000,
        left_context_ms: 5_000,
        right_context_ms: 5_000,
        timeline_contract: "source-audio-clock-v1".into(),
    }
}

pub(crate) fn for_task(task: SpeechTask) -> TranslationProfile {
    TranslationProfile {
        task,
        ..selected_profile()
    }
}
/// New requests use speech detection. Previously stored profiles remain registered.
pub(crate) fn default_profile() -> TranslationProfile {
    default_for_task(SpeechTask::TranslateToEnglish)
}
pub(crate) fn default_for_task(task: SpeechTask) -> TranslationProfile {
    for_quality(task, crate::ai_subtitles::settings::Quality::Balanced)
}

pub(crate) fn for_quality(
    task: SpeechTask,
    quality: crate::ai_subtitles::settings::Quality,
) -> TranslationProfile {
    use crate::ai_subtitles::settings::Quality;
    let spec = model_spec_for_quality(quality);
    TranslationProfile {
        model_id: spec.id,
        model_sha256: spec.sha256,
        pipeline_revision: match quality {
            Quality::Fast => "translation-window-v2-fast",
            Quality::Balanced | Quality::Best => "translation-window-v2",
        }
        .into(),
        ..for_task(task)
    }
}

pub(crate) fn legacy_profiles() -> [TranslationProfile; 2] {
    [selected_profile(), for_task(SpeechTask::Transcribe)]
}
pub(crate) fn profiles() -> Vec<TranslationProfile> {
    use crate::ai_subtitles::settings::Quality;
    let mut registered = legacy_profiles().to_vec();
    for quality in [Quality::Balanced, Quality::Fast, Quality::Best] {
        for task in [SpeechTask::TranslateToEnglish, SpeechTask::Transcribe] {
            registered.push(for_quality(task, quality));
        }
    }
    registered
}
pub(crate) fn registered(profile: &TranslationProfile) -> bool {
    profiles().iter().any(|candidate| candidate == profile)
}
/// An exact registered profile determines its immutable model artifact.
pub(crate) fn model_spec_for(
    profile: &TranslationProfile,
) -> Result<CatalogModel, plurx_core::error::StoreError> {
    model_specs()
        .into_iter()
        .find(|spec| {
            spec.id == profile.model_id
                && spec.sha256 == profile.model_sha256
                && registered(profile)
        })
        .ok_or_else(|| {
            plurx_core::error::StoreError::Task("translation_profile_unavailable".into())
        })
}
pub(crate) fn model_specs() -> [CatalogModel; 2] {
    [
        model_spec(),
        model_spec_for_quality(crate::ai_subtitles::settings::Quality::Best),
    ]
}

fn model_spec_for_quality(quality: crate::ai_subtitles::settings::Quality) -> CatalogModel {
    let id = match quality {
        crate::ai_subtitles::settings::Quality::Best => "large-v3-q5_0",
        _ => "small",
    };
    crate::subtitle_models::catalog()
        .models
        .iter()
        .find(|model| model.id == id)
        .expect("registered speech tier")
        .clone()
}

/// Task-specific production-path evidence using the publisher's German words.
/// This is a bounded smoke qualification, not a broad recognition quality claim.
pub(crate) fn transcription_self_test(cues: &[TranslationCue]) -> bool {
    if cues.is_empty() || cues.len() > 128 {
        return false;
    }
    let text = cues
        .iter()
        .map(|cue| cue.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if text.len() > 16 * 1024 {
        return false;
    }
    let lower = text.to_lowercase();
    let words = lower
        .split(|c: char| !c.is_alphanumeric())
        .collect::<std::collections::BTreeSet<_>>();
    [
        "slalom",
        "ersten",
        "lauf",
        "teilnehmer",
        "rennen",
        "ergebnis",
    ]
    .iter()
    .all(|word| words.contains(word))
}

/// Vocabulary51865 has99 language codes. These are technical model facts,
/// not99 measured translation-quality qualifications; Cantonese/yue requires
/// the later51866 vocabulary and must not be advertised for this Small model.
pub(crate) fn model_spec() -> CatalogModel {
    CatalogModel {
        id: "small".into(),
        label: "Small multilingual (experimental translation)".into(),
        languages: plurx_core::store::background_jobs_transcription::WHISPER_LANGUAGES
            .iter().filter(|language| **language != "yue").map(|value| (*value).into()).collect(),
        download_bytes: 487_601_967,
        sha256: MODEL_SHA256.into(),
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-small.bin".into(),
        source: "https://huggingface.co/ggerganov/whisper.cpp/tree/5359861c739e955e79d9a303bcbc70fb988958b1".into(),
        license: "MIT".into(),
    }
}

/// Bounded recognizer-path evidence, not an automatic translation quality test.
/// Strong English words absent from the German source prevent a plain German
/// transcription (including borrowed sports terms) from qualifying translate.
pub(crate) fn english_self_test(cues: &[TranslationCue]) -> bool {
    if cues.is_empty() || cues.len() > 128 {
        return false;
    }
    let text = cues
        .iter()
        .map(|cue| cue.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if text.len() > 16 * 1024 {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    let words: std::collections::BTreeSet<_> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|value| !value.is_empty())
        .collect();
    ["first", "run", "race", "slalom", "finish", "36"]
        .iter()
        .all(|word| words.contains(word))
        && (words.contains("116") || words.contains("116th"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn registered_small_profiles_resolve_exact_bytes_and_preserve_legacy_identity() {
        let legacy = br#"{"adapter_contract":"whisper.cpp-1.9.2-cpu-v1","core_ms":30000,"left_context_ms":5000,"model_id":"small","model_sha256":"1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b","pipeline_revision":"translation-window-v1","right_context_ms":5000,"timeline_contract":"source-audio-clock-v1","version":1}"#;
        assert_eq!(
            plurx_core::store::subtitle_translation::canonical_bytes(&selected_profile())
                .expect("legacy bytes")
                .as_slice(),
            legacy
        );
        let ids = profiles()
            .into_iter()
            .map(|profile| {
                assert!(registered(&profile));
                let model = model_spec_for(&profile).expect("exact registered model");
                assert_eq!(model.id, profile.model_id);
                assert_eq!(model.sha256, profile.model_sha256);
                if model.id == "small" {
                    assert_eq!(model.languages.len(), 99);
                    assert!(!model.languages.iter().any(|language| language == "yue"));
                    assert_eq!(model.download_bytes, 487_601_967);
                } else {
                    assert_eq!(model.id, "large-v3-q5_0");
                }
                profile.id().expect("profile identity")
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 8);
        assert!(registered(&default_profile()));
        assert_eq!(for_task(SpeechTask::Transcribe).model_id, "small");
        let mut crosswired = default_profile();
        crosswired.model_sha256 = "0".repeat(64);
        assert!(!registered(&crosswired));
        assert!(model_spec_for(&crosswired).is_err());
        let mut unsupported_model = default_profile();
        unsupported_model.model_id = "base".into();
        unsupported_model.model_sha256 =
            "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe".into();
        assert!(!registered(&unsupported_model));
        assert!(model_spec_for(&unsupported_model).is_err());
        let mut unregistered = default_profile();
        unregistered.core_ms = 1_000;
        assert!(model_spec_for(&unregistered).is_err());
    }

    #[test]
    fn tasks_keep_legacy_profile_bytes_and_qualify_independently() {
        let translation = selected_profile();
        let mut legacy = serde_json::to_value(&translation).expect("profile JSON");
        assert!(legacy.get("task").is_none());
        let restored: TranslationProfile =
            serde_json::from_value(legacy.clone()).expect("legacy profile");
        assert_eq!(
            restored.id().expect("restored identity"),
            translation.id().expect("translation identity")
        );
        legacy["task"] = serde_json::json!("transcribe");
        let transcription: TranslationProfile =
            serde_json::from_value(legacy).expect("transcription profile");
        assert!(registered(&transcription));
        assert_ne!(
            translation.id().expect("translation identity"),
            transcription.id().expect("transcription identity")
        );
        assert_eq!(transcription.task.output_language("de"), "de");
        let cue = TranslationCue { cue_id: "unused".into(), start_ms: 0, end_ms: 1_000,
            text: "Ihr erstes war der Slalom, bei dem sie in ihrem ersten Lauf ein Did not Finish erhielt. 36 der 116 Teilnehmer erzielten in diesem Rennen das gleiche Ergebnis.".into() };
        assert!(transcription_self_test(std::slice::from_ref(&cue)));
        assert!(!english_self_test(&[cue]));
    }

    #[test]
    fn selected_profile_and_fixture_match_qualified_pins_without_yue() {
        selected_profile().validate().expect("test setup");
        let model = model_spec();
        assert_eq!(model.languages.len(), 99);
        assert!(!model.languages.iter().any(|language| language == "yue"));
        assert_eq!(model.sha256, selected_profile().model_sha256);
        assert_eq!(
            hex::encode(Sha256::digest(SELF_TEST_FIXTURE)),
            SELF_TEST_SHA256
        );
        assert!(SELF_TEST_ATTRIBUTION.contains("creativecommons.org/licenses/by/4.0/"));
    }

    #[test]
    fn english_self_test_rejects_source_language_sports_loanwords_and_empty_output() {
        let cue = |text: &str| TranslationCue {
            cue_id: "unused".into(),
            start_ms: 0,
            end_ms: 1_000,
            text: text.into(),
        };
        assert!(!english_self_test(&[]));
        assert!(!english_self_test(&[cue(
            "Slalom, Did Not Finish,36 der116Teilnehmer im ersten Lauf."
        )]));
        assert!(english_self_test(&[cue("The first was slalom. Did not finish in her first run.36 of the 116th participant achieved the same result in this race.")]));
    }
}
