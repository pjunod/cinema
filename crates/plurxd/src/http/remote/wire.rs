//! Exact public remote DTOs; transport proofs are headers, never state fields.
use plurx_core::remote_control::{
    Acknowledgement, Command, Credit, Target, TrackKind, Version, MAX_SAFE_INTEGER,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! dto { ($name:ident {$($(#[$attr:meta])* $field:ident:$ty:ty),* $(,)?}) => {
    #[derive(Clone,Serialize,Deserialize)] #[serde(deny_unknown_fields)]
    pub(crate) struct $name { pub version:Version, $($(#[$attr])* pub $field:$ty),* }
}; }
fn nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}
dto!(CreateReceiver {
    name: String,
    platform: Platform
});
dto!(CreateSession {
    receiver_id: Uuid,
    foreground_id: Uuid
});
dto!(Presence {
    target: Target,
    state: ReceiverState
});
dto!(Poll {
    target: Target,
    after_delivery_id: u64,
    after_response_revision: u64,
    wait_ms: u64
});
dto!(Ack {target:Target,outcomes:Vec<Acknowledgement>});
dto!(ReadState {
    target: Target,
    grant_id: Uuid,
    after_revision: u64,
    wait_ms: u64
});
dto!(ControlRequest {target:Target,grant_id:Uuid,action:ControlAction,#[serde(deserialize_with="nullable")] control_epoch:Option<Uuid>});
dto!(PairStart { target: Target });
dto!(PairClaim {
    target: Target,
    challenge_id: Option<Uuid>,
    code: String,
    controller_name: String
});
dto!(PairApprove {
    target: Target,
    pending_id: Uuid,
    approve: bool
});
dto!(PairResult {
    target: Target,
    pending_id: Uuid
});
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Platform {
    Web,
    AppleTv,
    AndroidTv,
    Desktop,
}
impl Platform {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::AppleTv => "apple_tv",
            Self::AndroidTv => "android_tv",
            Self::Desktop => "desktop",
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControlAction {
    Acquire,
    Renew,
    Takeover,
    Release,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Route {
    Home,
    Library,
    Details,
    Search,
    Playback,
    Tracks,
    Restricted,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Track {
    pub kind: TrackKind,
    pub option_id: String,
    pub label: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum MediaIdentity {
    Item { item_id: u64 },
    LiveChannel { channel_id: String },
}
impl MediaIdentity {
    fn valid(&self) -> bool {
        match self {
            Self::Item { item_id } => positive(*item_id),
            Self::LiveChannel { channel_id } => label(channel_id, 128),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlaybackSummary {
    pub media: MediaIdentity,
    pub title: String,
    pub playing: bool,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub tracks: Vec<Track>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReceiverState {
    pub state_revision: u64,
    pub context_revision: u64,
    pub focus_revision: u64,
    pub route: Route,
    pub capabilities: Vec<String>,
    pub focused_label: Option<String>,
    pub credits: Vec<Credit>,
    pub text_nonce: Option<Uuid>,
    pub playback: Option<PlaybackSummary>,
}
pub(crate) fn label(s: &str, cap: usize) -> bool {
    !s.is_empty() && s.len() <= cap && !s.chars().any(char::is_control)
}
pub(crate) fn safe(v: u64) -> bool {
    v <= MAX_SAFE_INTEGER
}
pub(crate) fn positive(v: u64) -> bool {
    v > 0 && safe(v)
}
impl ReceiverState {
    pub fn validate(&mut self) -> bool {
        if ![
            self.state_revision,
            self.context_revision,
            self.focus_revision,
        ]
        .into_iter()
        .all(positive)
            || self.capabilities.len() > 12
            || self.credits.len() > 16
        {
            return false;
        }
        const ACTIONS: [&str; 12] = [
            "navigate",
            "select",
            "back",
            "home",
            "set_playing",
            "seek_relative",
            "seek_absolute",
            "stop",
            "open_tracks",
            "choose_track",
            "text_replace",
            "play_item",
        ];
        if self
            .capabilities
            .iter()
            .any(|a| !ACTIONS.contains(&a.as_str()))
            || self.focused_label.as_ref().is_some_and(|s| !label(s, 256))
        {
            return false;
        }
        if let Some(p) = &self.playback {
            if !p.media.valid()
                || !label(&p.title, 256)
                || !safe(p.position_ms)
                || !safe(p.duration_ms)
                || p.tracks.len() > 64
                || p.tracks
                    .iter()
                    .any(|t| !label(&t.option_id, 128) || !label(&t.label, 256))
            {
                return false;
            }
        }
        if self.route == Route::Restricted {
            self.capabilities.clear();
            self.credits.clear();
            self.focused_label = None;
            self.text_nonce = None;
            self.playback = None;
        }
        true
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "body",
    deny_unknown_fields,
    rename_all = "snake_case"
)]
pub(crate) enum Request {
    CreateReceiver(CreateReceiver),
    CreateSession(CreateSession),
    Presence(Presence),
    Poll(Poll),
    Ack(Ack),
    ReadState(ReadState),
    Control(ControlRequest),
    Commands(Command),
    PairStart(PairStart),
    PairClaim(PairClaim),
    PairApprove(PairApprove),
    PairResult(PairResult),
    ListSessions { version: Version },
}
impl Request {
    pub fn target(&self) -> Option<&Target> {
        match self {
            Self::Presence(r) => Some(&r.target),
            Self::Poll(r) => Some(&r.target),
            Self::Ack(r) => Some(&r.target),
            Self::ReadState(r) => Some(&r.target),
            Self::Control(r) => Some(&r.target),
            Self::Commands(r) => Some(&r.target),
            Self::PairStart(r) => Some(&r.target),
            Self::PairClaim(r) => Some(&r.target),
            Self::PairApprove(r) => Some(&r.target),
            Self::PairResult(r) => Some(&r.target),
            _ => None,
        }
    }
    pub fn grant_id(&self) -> Option<Uuid> {
        match self {
            Self::ReadState(r) => Some(r.grant_id),
            Self::Control(r) => Some(r.grant_id),
            Self::Commands(r) => Some(r.grant_id),
            _ => None,
        }
    }
    pub fn receiver_side(&self) -> bool {
        matches!(
            self,
            Self::CreateSession(_)
                | Self::Presence(_)
                | Self::Poll(_)
                | Self::Ack(_)
                | Self::PairStart(_)
                | Self::PairApprove(_)
        )
    }
    pub fn wait_ms(&self) -> u64 {
        match self {
            Self::Poll(r) => r.wait_ms,
            Self::ReadState(r) => r.wait_ms,
            _ => 0,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Proof {
    pub receiver_hash: Option<String>,
    pub grant_hash: Option<String>,
    pub pairing_hash: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Dispatch {
    pub version: Version,
    pub token_digest: String,
    pub user_id: i64,
    pub proof: Proof,
    pub request: Request,
}
#[derive(Clone, Serialize)]
pub(crate) struct Control {
    pub control_epoch: Uuid,
    pub active_grant_id: Uuid,
    pub controller_name: String,
}
