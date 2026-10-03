//! Closed file revisions and server-only current file snapshot witnesses.
use crate::{
    error::StoreError,
    secrets::{CredentialKey, SealedSecret, SharingSecretPurpose},
    sharing::{invalid, SharingIdentity, SourceId},
};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;
use zeroize::Zeroize;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct FileRevision(String);
impl FileRevision {
    pub fn parse(value: &str) -> Result<Self, StoreError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid());
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for FileRevision {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = FileRevision;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a canonical opaque file revision")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                FileRevision::parse(value).map_err(|_| E::custom("invalid file revision"))
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

/// Never serialized or formatted. Only a current authorized Store query may
/// construct this exact private projection. A wire digest is not this witness.
#[derive(Clone)]
pub struct SourceFileWitness {
    pub(crate) server: Uuid,
    pub(crate) epoch: Uuid,
    pub(crate) library: SourceId,
    pub(crate) item: SourceId,
    pub(crate) file: SourceId,
    projection: String,
}
impl SourceFileWitness {
    pub(crate) fn from_current_projection(
        server: Uuid,
        epoch: Uuid,
        library: SourceId,
        item: SourceId,
        file: SourceId,
        projection: String,
    ) -> Result<Self, StoreError> {
        if projection.is_empty() || projection.len() > 2 * 1024 * 1024 {
            return Err(invalid());
        }
        Ok(Self {
            server,
            epoch,
            library,
            item,
            file,
            projection,
        })
    }
    /// The admission transaction must regenerate and compare this projection
    /// together with the bound identities, not accept an old hash alone.
    pub(crate) fn canonical_projection(&self) -> &str {
        &self.projection
    }
}

/// Stable random purpose material persisted sealed to a Source/epoch. No raw
/// key export, Serialize or Debug implementation. Sealing-master rewrap keeps
/// this material; peer credential rotation never changes it.
pub struct CatalogueRevisionKey {
    server: Uuid,
    epoch: Uuid,
    key: [u8; 32],
}
impl Drop for CatalogueRevisionKey {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(DIGITS[usize::from(byte >> 4)] as char);
        value.push(DIGITS[usize::from(byte & 15)] as char);
    }
    value
}
impl CatalogueRevisionKey {
    /// Cryptographic factory only. Qualified schema activation owns durable
    /// insertion; catalogue reads must never call this factory to repair state.
    pub fn generate_sealed(
        credential: &CredentialKey,
        identity: SharingIdentity,
    ) -> Result<SealedSecret, StoreError> {
        let mut bytes = zeroize::Zeroizing::new([0_u8; 32]);
        getrandom::getrandom(bytes.as_mut()).map_err(|_| invalid())?;
        let clear = zeroize::Zeroizing::new(hex(bytes.as_ref()));
        credential
            .seal_sharing(
                SharingSecretPurpose::CatalogueRevision,
                identity.server_id,
                identity.catalogue_epoch,
                &clear,
            )
            .map_err(|_| invalid())
    }
    pub fn open(
        credential: &CredentialKey,
        identity: SharingIdentity,
        envelope: &SealedSecret,
    ) -> Result<Self, StoreError> {
        let clear = credential
            .open_sharing(
                SharingSecretPurpose::CatalogueRevision,
                identity.server_id,
                identity.catalogue_epoch,
                envelope,
            )
            .map_err(|_| invalid())?;
        if clear.expose().len() != 64
            || !clear
                .expose()
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid());
        }
        let mut key = [0_u8; 32];
        for (index, byte) in key.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&clear.expose()[index * 2..index * 2 + 2], 16)
                .map_err(|_| invalid())?;
        }
        Ok(Self {
            server: identity.server_id,
            epoch: identity.catalogue_epoch,
            key,
        })
    }
    pub fn file_revision(&self, witness: &SourceFileWitness) -> Result<FileRevision, StoreError> {
        if self.server != witness.server || self.epoch != witness.epoch {
            return Err(invalid());
        }
        let root = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &self.key);
        let derived = ring::hmac::sign(&root, b"cinema-sharing-file-revision-key-v1");
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, derived.as_ref());
        let mut mac = ring::hmac::Context::with_key(&key);
        mac.update(b"cinema-sharing-file-revision-v1\0");
        mac.update(witness.server.as_bytes());
        mac.update(witness.epoch.as_bytes());
        for id in [&witness.library, &witness.item, &witness.file] {
            mac.update(id.as_str().as_bytes());
            mac.update(&[0]);
        }
        mac.update(witness.canonical_projection().as_bytes());
        FileRevision::parse(&hex(mac.sign().as_ref()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn witness(identity: &SharingIdentity, projection: &str) -> SourceFileWitness {
        SourceFileWitness::from_current_projection(
            identity.server_id,
            identity.catalogue_epoch,
            SourceId::parse("1").expect("library"),
            SourceId::parse("9007199254740993").expect("item"),
            SourceId::parse("9223372036854775807").expect("file"),
            projection.to_owned(),
        )
        .expect("current private fixture")
    }
    #[test]
    fn sharing_catalogue_file_revision_is_stable_through_rewrap_and_binds_private_snapshot() {
        let identity = SharingIdentity {
            server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            created_at_ms: 1000,
        };
        let old = CredentialKey::from_bytes([19; 32]);
        let new = CredentialKey::from_bytes([20; 32]);
        let envelope =
            CatalogueRevisionKey::generate_sealed(&old, identity.clone()).expect("purpose factory");
        let revision_key = CatalogueRevisionKey::open(&old, identity.clone(), &envelope)
            .expect("purpose material");
        let current = witness(
            &identity,
            "[1,\"/private/synthetic.mkv\",20,1000,\"exact probe snapshot\"]",
        );
        let revision = revision_key.file_revision(&current).expect("revision");
        let clear = old
            .open_sharing(
                SharingSecretPurpose::CatalogueRevision,
                identity.server_id,
                identity.catalogue_epoch,
                &envelope,
            )
            .expect("rewrap input");
        let rewrapped = new
            .seal_sharing(
                SharingSecretPurpose::CatalogueRevision,
                identity.server_id,
                identity.catalogue_epoch,
                clear.expose(),
            )
            .expect("rewrap envelope");
        let replacement = CatalogueRevisionKey::open(&new, identity.clone(), &rewrapped)
            .expect("same stable material");
        assert_eq!(
            revision,
            replacement
                .file_revision(&current)
                .expect("stable revision")
        );
        assert_ne!(
            revision,
            replacement
                .file_revision(&witness(&identity, "changed private probe/path snapshot"))
                .expect("changed revision")
        );
        let mut other = witness(&identity, current.canonical_projection());
        other.file = SourceId::parse("2").expect("other file");
        assert_ne!(
            revision,
            replacement.file_revision(&other).expect("bound file")
        );
        other.epoch = Uuid::new_v4();
        assert!(replacement.file_revision(&other).is_err());
        for invalid in ["A".repeat(64), "0".repeat(65), "/private/path".into()] {
            assert!(
                serde_json::from_value::<FileRevision>(serde_json::Value::String(invalid)).is_err()
            );
        }
        assert!(!serde_json::to_string(&revision)
            .expect("wire revision")
            .contains("private"));
    }
}

pub const MAX_DETAIL_FILES: usize = 64;
const MAX_SAFE: i64 = 9_007_199_254_740_991;
fn text(value: &str, max: usize) -> bool {
    value.len() <= max && !value.chars().any(char::is_control)
}
fn number(value: Option<i64>, max: i64) -> bool {
    value.is_none_or(|value| (0..=max).contains(&value))
}
fn bounded<'de, T: Deserialize<'de>, D: Deserializer<'de>, const N: usize>(
    deserializer: D,
) -> Result<Vec<T>, D::Error> {
    struct Visitor<T, const N: usize>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const N: usize> serde::de::Visitor<'de> for Visitor<T, N> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "at most {N} closed records")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> Result<Vec<T>, A::Error> {
            let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(N));
            while let Some(value) = sequence.next_element()? {
                if values.len() == N {
                    return Err(serde::de::Error::custom("too many sharing detail records"));
                }
                values.push(value);
            }
            Ok(values)
        }
    }
    deserializer.deserialize_seq(Visitor::<T, N>(std::marker::PhantomData))
}
fn files<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<SourcePlayableFile>, D::Error> {
    bounded::<_, _, 64>(d)
}
fn audio<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<SourceAudioTrack>, D::Error> {
    bounded::<_, _, 64>(d)
}
fn subtitles<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<SourceSubtitleTrack>, D::Error> {
    bounded::<_, _, 128>(d)
}
fn chapters<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<SourceChapter>, D::Error> {
    bounded::<_, _, 1024>(d)
}
fn skips<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<SourceSkipRegion>, D::Error> {
    bounded::<_, _, 32>(d)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceItemDetails {
    pub item: crate::sharing_catalogue::SourceCatalogueItem,
    #[serde(deserialize_with = "files")]
    pub files: Vec<SourcePlayableFile>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAudioTrack {
    pub index: i64,
    pub codec: String,
    pub channels: Option<i64>,
    pub sample_rate: Option<i64>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub default: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSubtitleTrack {
    pub index: i64,
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub default: bool,
    pub forced: bool,
    pub hearing_impaired: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceChapter {
    pub index: i64,
    pub title: String,
    pub start_ms: i64,
    pub end_ms: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSkipRegion {
    pub kind: crate::segplan::AnnotationKind,
    pub start_ms: i64,
    pub end_ms: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDolbyVision {
    pub profile: Option<i64>,
    pub level: Option<i64>,
    pub bl_compat_id: Option<i64>,
    pub el_present: Option<bool>,
    pub rpu_present: Option<bool>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePlayableFile {
    pub file_id: SourceId,
    pub revision: FileRevision,
    pub size: String,
    pub duration_ms: Option<i64>,
    pub container: Option<String>,
    pub video_codec: Option<String>,
    pub video_codec_tag: Option<String>,
    pub video_profile: Option<String>,
    pub field_order: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub bit_depth: Option<i64>,
    pub hdr: Option<String>,
    pub hdr_format: Option<String>,
    pub bitrate: Option<i64>,
    pub max_cll: Option<i64>,
    pub max_fall: Option<i64>,
    pub mastering_max_luminance: Option<i64>,
    pub luminance_source: Option<String>,
    pub dolby_vision: SourceDolbyVision,
    pub audio_offset_ms: i64,
    pub probed: bool,
    #[serde(deserialize_with = "audio")]
    pub audio_streams: Vec<SourceAudioTrack>,
    #[serde(deserialize_with = "subtitles")]
    pub subtitle_streams: Vec<SourceSubtitleTrack>,
    #[serde(deserialize_with = "chapters")]
    pub chapters: Vec<SourceChapter>,
    #[serde(deserialize_with = "skips")]
    pub skip_regions: Vec<SourceSkipRegion>,
}
impl SourcePlayableFile {
    pub fn validate(&self) -> Result<(), StoreError> {
        let size = self.size.parse::<i64>().map_err(|_| invalid())?;
        if size < 0
            || size.to_string() != self.size
            || !number(self.duration_ms, MAX_SAFE)
            || !number(self.width, 65535)
            || !number(self.height, 65535)
            || !number(self.bit_depth, 64)
            || !number(self.bitrate, MAX_SAFE)
            || !number(self.max_cll, MAX_SAFE)
            || !number(self.max_fall, MAX_SAFE)
            || !number(self.mastering_max_luminance, MAX_SAFE)
            || !(-MAX_SAFE..=MAX_SAFE).contains(&self.audio_offset_ms)
            || !number(self.dolby_vision.profile, 255)
            || !number(self.dolby_vision.level, 255)
            || !number(self.dolby_vision.bl_compat_id, 255)
            || [
                &self.container,
                &self.video_codec,
                &self.video_codec_tag,
                &self.video_profile,
                &self.field_order,
                &self.hdr,
                &self.hdr_format,
                &self.luminance_source,
            ]
            .iter()
            .any(|value| value.as_ref().is_some_and(|value| !text(value, 128)))
            || self.audio_streams.len() > 64
            || self.subtitle_streams.len() > 128
            || self.chapters.len() > 1024
            || self.skip_regions.len() > 32
        {
            return Err(invalid());
        }
        if self
            .audio_streams
            .iter()
            .map(|t| t.index)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != self.audio_streams.len()
            || self
                .subtitle_streams
                .iter()
                .map(|t| t.index)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.subtitle_streams.len()
            || self
                .chapters
                .iter()
                .map(|t| t.index)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.chapters.len()
        {
            return Err(invalid());
        }
        let track = |index: i64, codec: &str, language: &Option<String>, title: &Option<String>| {
            (0..=4095).contains(&index)
                && text(codec, 64)
                && language.as_ref().is_none_or(|v| text(v, 64))
                && title.as_ref().is_none_or(|v| text(v, 512))
        };
        if self.audio_streams.iter().any(|a| {
            !track(a.index, &a.codec, &a.language, &a.title)
                || !number(a.channels, 128)
                || !number(a.sample_rate, 1_000_000)
        }) || self
            .subtitle_streams
            .iter()
            .any(|s| !track(s.index, &s.codec, &s.language, &s.title))
        {
            return Err(invalid());
        }
        if self.chapters.iter().any(|c| {
            !(0..=4095).contains(&c.index)
                || !text(&c.title, 512)
                || !(0..=MAX_SAFE).contains(&c.start_ms)
                || !(c.start_ms + 1..=MAX_SAFE).contains(&c.end_ms)
        }) || self.skip_regions.iter().any(|r| {
            !(0..=MAX_SAFE).contains(&r.start_ms)
                || !(r.start_ms + 1..=MAX_SAFE).contains(&r.end_ms)
        }) {
            return Err(invalid());
        }
        Ok(())
    }
}
impl SourceItemDetails {
    pub fn validate(&self) -> Result<(), StoreError> {
        self.item.validate().map_err(|_| invalid())?;
        if self.files.len() > MAX_DETAIL_FILES
            || self
                .files
                .iter()
                .map(|file| &file.file_id)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.files.len()
        {
            return Err(invalid());
        }
        self.files.iter().try_for_each(SourcePlayableFile::validate)
    }
}

impl SourceFileWitness {
    /// Explicit presentation whitelist derived from the same private snapshot
    /// as the revision. No filesystem, probe document or caption body is copied.
    pub fn playable_file(
        &self,
        key: &CatalogueRevisionKey,
    ) -> Result<SourcePlayableFile, StoreError> {
        let v: serde_json::Value = serde_json::from_str(&self.projection).map_err(|_| invalid())?;
        if v.as_array().is_none_or(|a| a.len() != 36) {
            return Err(invalid());
        }
        let integer = |index: usize| -> Result<Option<i64>, StoreError> {
            if v[index].is_null() {
                Ok(None)
            } else {
                v[index].as_i64().map(Some).ok_or_else(invalid)
            }
        };
        let string = |index: usize| -> Result<Option<String>, StoreError> {
            if v[index].is_null() {
                Ok(None)
            } else {
                v[index]
                    .as_str()
                    .map(|s| Some(s.to_owned()))
                    .ok_or_else(invalid)
            }
        };
        let boolean = |index: usize| -> Result<Option<bool>, StoreError> {
            integer(index)?
                .map(|n| match n {
                    0 => Ok(false),
                    1 => Ok(true),
                    _ => Err(invalid()),
                })
                .transpose()
        };
        let audio: Vec<crate::domain::AudioStream> =
            serde_json::from_str(v[21].as_str().unwrap_or("[]")).map_err(|_| invalid())?;
        let subtitles: Vec<crate::domain::SubtitleStream> =
            serde_json::from_str(v[22].as_str().unwrap_or("[]")).map_err(|_| invalid())?;
        let mut chapters = Vec::new();
        if let Some(probe) = v[23].as_str() {
            let probe: serde_json::Value = serde_json::from_str(probe).map_err(|_| invalid())?;
            if let Some(values) = probe.get("chapters").and_then(serde_json::Value::as_array) {
                if values.len() > 1024 {
                    return Err(invalid());
                }
                for (ordinal, c) in values.iter().enumerate() {
                    let time = |field: &str| -> Result<i64, StoreError> {
                        let seconds = c[field]
                            .as_str()
                            .and_then(|s| s.parse::<f64>().ok())
                            .or_else(|| c[field].as_f64())
                            .ok_or_else(invalid)?;
                        let ms = seconds * 1000.0;
                        if !ms.is_finite() || !(0.0..=MAX_SAFE as f64).contains(&ms) {
                            return Err(invalid());
                        }
                        Ok(ms.round() as i64)
                    };
                    chapters.push(SourceChapter {
                        index: ordinal as i64,
                        title: c["tags"]["title"].as_str().unwrap_or("").to_owned(),
                        start_ms: time("start_time")?,
                        end_ms: time("end_time")?,
                    });
                }
            }
        }
        let file = SourcePlayableFile {
            file_id: self.file.clone(),
            revision: key.file_revision(self)?,
            size: integer(7)?.ok_or_else(invalid)?.to_string(),
            duration_ms: integer(9)?,
            container: string(10)?,
            video_codec: string(11)?,
            video_profile: string(12)?,
            video_codec_tag: string(13)?,
            field_order: string(14)?,
            width: integer(15)?,
            height: integer(16)?,
            bit_depth: integer(17)?,
            hdr: string(18)?,
            hdr_format: string(19)?,
            bitrate: integer(20)?,
            audio_offset_ms: integer(25)?.unwrap_or(0),
            dolby_vision: SourceDolbyVision {
                profile: integer(26)?,
                level: integer(27)?,
                bl_compat_id: integer(28)?,
                el_present: boolean(29)?,
                rpu_present: boolean(30)?,
            },
            max_cll: integer(31)?,
            max_fall: integer(32)?,
            mastering_max_luminance: integer(33)?,
            luminance_source: string(34)?,
            probed: !v[23].is_null(),
            audio_streams: audio
                .into_iter()
                .map(|a| SourceAudioTrack {
                    index: a.index,
                    codec: a.codec,
                    channels: a.channels,
                    sample_rate: a.sample_rate,
                    language: a.language,
                    title: a.title,
                    default: a.default,
                })
                .collect(),
            subtitle_streams: subtitles
                .into_iter()
                .map(|a| SourceSubtitleTrack {
                    index: a.index,
                    codec: a.codec,
                    language: a.language,
                    title: a.title,
                    default: a.default,
                    forced: a.forced,
                    hearing_impaired: a.hearing_impaired,
                })
                .collect(),
            chapters,
            skip_regions: Vec::new(),
        };
        file.validate()?;
        Ok(file)
    }
}
