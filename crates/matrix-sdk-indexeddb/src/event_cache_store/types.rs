// Copyright 2025 The Matrix.org Foundation C.I.C.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License

use matrix_sdk_base::{
    deserialized_responses::TimelineEvent, event_cache::store::extract_event_relation,
    linked_chunk::ChunkIdentifier,
};
use ruma::OwnedEventId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct ChunkForCache {
    pub id: String,
    pub raw_id: u64,
    pub previous: Option<String>,
    pub raw_previous: Option<u64>,
    pub next: Option<String>,
    pub raw_next: Option<u64>,
    pub type_str: String,
}

impl ChunkForCache {
    /// Used to set field `type_str` to represent a chunk that contains events
    pub const CHUNK_TYPE_EVENT_TYPE_STRING: &str = "E";
    /// Used to set field `type_str` to represent a chunk that is a gap
    pub const CHUNK_TYPE_GAP_TYPE_STRING: &str = "G";
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EventForCache {
    InBand(InBandEventForCache),
    OutOfBand(OutOfBandEventForCache),
}

impl EventForCache {
    pub fn take_content(self) -> TimelineEvent {
        match self {
            EventForCache::InBand(i) => i.content,
            EventForCache::OutOfBand(o) => o.content,
        }
    }

    pub fn replace_content(&mut self, content: TimelineEvent) -> TimelineEvent {
        match self {
            EventForCache::InBand(i) => std::mem::replace(&mut i.content, content),
            EventForCache::OutOfBand(o) => std::mem::replace(&mut o.content, content),
        }
    }
}

pub type InBandEventForCache = GenericEventForCache<PositionForCache>;
pub type OutOfBandEventForCache = GenericEventForCache<()>;

#[derive(Debug, Serialize, Deserialize)]
pub struct GenericEventForCache<P> {
    pub content: TimelineEvent,
    pub room_id: String,
    pub position: P,
}

impl<P> GenericEventForCache<P> {
    pub fn relation(&self) -> Option<(OwnedEventId, String)> {
        extract_event_relation(self.content.raw())
    }
}

#[derive(Debug, Default, Copy, Clone, Serialize, Deserialize)]
pub struct PositionForCache {
    pub chunk_id: u64,
    pub index: usize,
}

impl From<PositionForCache> for matrix_sdk_base::linked_chunk::Position {
    fn from(value: PositionForCache) -> Self {
        Self::new(ChunkIdentifier::new(value.chunk_id), value.index)
    }
}

impl From<matrix_sdk_base::linked_chunk::Position> for PositionForCache {
    fn from(value: matrix_sdk_base::linked_chunk::Position) -> Self {
        Self { chunk_id: value.chunk_identifier().index(), index: value.index() }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GapForCache {
    pub prev_token: String,
}
