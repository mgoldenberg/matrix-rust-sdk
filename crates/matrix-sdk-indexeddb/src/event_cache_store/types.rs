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

/// Representation of a [`Chunk`](matrix_sdk_base::linked_chunk::Chunk)
/// which can be stored in IndexedDB.
#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub struct Chunk {
    /// The identifier of the chunk - i.e.,
    /// [`ChunkIdentifier`](matrix_sdk_base::linked_chunk::ChunkIdentifier).
    pub identifier: u64,
    /// The previous chunk in the list.
    pub previous: Option<u64>,
    /// The next chunk in the list.
    pub next: Option<u64>,
    /// The type of the chunk.
    pub chunk_type: ChunkType,
}

/// The type of a [`Chunk`](Chunk)
#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkType {
    /// A chunk that holds events.
    Event,
    /// A chunk that represents a gap.
    Gap,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Event {
    InBand(InBandEvent),
    OutOfBand(OutOfBandEvent),
}

impl Event {
    pub fn take_content(self) -> TimelineEvent {
        match self {
            Event::InBand(i) => i.content,
            Event::OutOfBand(o) => o.content,
        }
    }

    pub fn replace_content(&mut self, content: TimelineEvent) -> TimelineEvent {
        match self {
            Event::InBand(i) => std::mem::replace(&mut i.content, content),
            Event::OutOfBand(o) => std::mem::replace(&mut o.content, content),
        }
    }
}

pub type InBandEvent = GenericEvent<Position>;
pub type OutOfBandEvent = GenericEvent<()>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenericEvent<P> {
    pub content: TimelineEvent,
    pub position: P,
}

impl<P> GenericEvent<P> {
    pub fn relation(&self) -> Option<(OwnedEventId, String)> {
        extract_event_relation(self.content.raw())
    }
}

#[derive(Debug, Default, Copy, Clone, Serialize, Deserialize)]
pub struct Position {
    pub chunk_id: u64,
    pub index: usize,
}

impl From<Position> for matrix_sdk_base::linked_chunk::Position {
    fn from(value: Position) -> Self {
        Self::new(ChunkIdentifier::new(value.chunk_id), value.index)
    }
}

impl From<matrix_sdk_base::linked_chunk::Position> for Position {
    fn from(value: matrix_sdk_base::linked_chunk::Position) -> Self {
        Self { chunk_id: value.chunk_identifier().index(), index: value.index() }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Gap {
    pub prev_token: String,
}
