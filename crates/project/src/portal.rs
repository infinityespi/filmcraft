//! A sequence's link to a Portals review: which uploads it made and which marker each note became.

use std::collections::BTreeMap;

use filmcraft_time::{FrameRate, Tick};
use serde::{Deserialize, Serialize};

use crate::MarkerId;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PortalLink {
    pub server: String,
    pub token: String,
    pub url: String,
    #[serde(default)]
    pub versions: Vec<PortalVersionLink>,
    /// Server comment id → the marker made from it. A note stays here after its marker is deleted,
    /// so syncing never brings back a marker the editor removed.
    #[serde(default)]
    pub notes: BTreeMap<String, PortalNote>,
}

/// One upload: where its first frame sits on the timeline, and the cut it was made from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PortalVersionLink {
    pub id: String,
    pub number: i64,
    pub start: Tick,
    pub frame_rate: FrameRate,
    /// Fingerprint of the tracks when uploaded, to tell when the cut has changed since.
    pub timeline_hash: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortalNote {
    pub marker: MarkerId,
    /// The text last written from the server. A marker that no longer matches was edited by hand.
    pub text: String,
    /// Kept in front of the text, such as "[v1] ", decided when the marker was made.
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub resolved: bool,
}
