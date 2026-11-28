use crate::http::hls::subtitle_names::{language_name, subtitle_characteristics, track_is_forced};
use crate::subtitle_transcription::normalized_language;
use plurx_core::domain::{AudioStream, MediaFile, SubtitleStream};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Route {
    #[default]
    Auto,
    Text,
    Audio,
}

pub(crate) enum Source<'a> {
    Text(&'a SubtitleStream, String),
    Audio(&'a AudioStream, Option<String>),
}

impl Source<'_> {
    pub(super) fn route(&self) -> Route {
        match self {
            Self::Text(..) => Route::Text,
            Self::Audio(..) => Route::Audio,
        }
    }

    pub(super) fn label(&self) -> String {
        match self {
            Self::Text(track, _) => {
                format!(
                    "Translate from the {} subtitles",
                    language_name(track.language.as_deref())
                )
            }
            Self::Audio(..) => "Create from the audio".to_owned(),
        }
    }
}

fn audio_track(file: &MediaFile) -> Option<&AudioStream> {
    file.audio_streams
        .iter()
        .find(|track| track.default)
        .or_else(|| file.audio_streams.first())
}

fn full_text(track: &SubtitleStream) -> bool {
    !track_is_forced(track) && !plurx_core::tracks::is_bitmap_subtitle(&track.codec)
}

pub(crate) fn existing_english(file: &MediaFile) -> Option<i64> {
    file.subtitle_streams
        .iter()
        .filter(|track| {
            full_text(track)
                && track
                    .language
                    .as_deref()
                    .is_some_and(|tag| normalized_language(tag) == "en")
        })
        .min_by_key(|track| {
            (
                subtitle_characteristics(track).is_some(),
                !track.default,
                track.index,
            )
        })
        .map(|track| track.index)
}

pub(crate) fn candidates(
    file: &MediaFile,
    route: Route,
    audio_index: Option<i64>,
) -> Vec<Source<'_>> {
    let audio = match audio_index {
        Some(index) => file.audio_streams.iter().find(|track| track.index == index),
        None => audio_track(file),
    };
    let audio_language = audio
        .and_then(|track| track.language.as_deref())
        .map(normalized_language)
        .filter(|language| {
            plurx_core::store::background_jobs_transcription::whisper_language(language)
        });
    let mut choices = Vec::new();
    if route != Route::Audio {
        let mut tracks = file
            .subtitle_streams
            .iter()
            .filter(|track| full_text(track))
            .filter_map(|track| {
                let language = crate::subtitle_text_translation::normalize_language(
                    track.language.as_deref()?,
                )
                .ok()?;
                Some((track, language))
            })
            .collect::<Vec<_>>();
        tracks.sort_by_key(|(track, language)| {
            (
                subtitle_characteristics(track).is_some(),
                audio_language.as_ref() != Some(language),
                !track.default,
                track.index,
            )
        });
        choices.extend(
            tracks
                .into_iter()
                .map(|(track, language)| Source::Text(track, language)),
        );
    }
    if route != Route::Text && crate::subtitle_transcription::eligible(file) {
        choices.extend(audio.map(|audio| Source::Audio(audio, audio_language)));
    }
    choices
}
