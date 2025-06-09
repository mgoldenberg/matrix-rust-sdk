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

/// An inclusive representation of an
/// [`Event`](matrix_sdk_base::event_cache::Event) which can be stored in
/// IndexedDB.
///
/// This is useful when (de)serializing an event which may either be in-band or
/// out-of-band.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Event {
    /// An in-band event, i.e., an event which is part of a chunk.
    InBand(InBandEvent),
    /// An out-of-band event, i.e., an event which is not part of a chunk.
    OutOfBand(OutOfBandEvent),
}

impl From<Event> for TimelineEvent {
    fn from(value: Event) -> Self {
        match value {
            Event::InBand(e) => e.content,
            Event::OutOfBand(e) => e.content,
        }
    }
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

/// A generic representation of an
/// [`Event`](matrix_sdk_base::event_cache::Event) which can be stored in
/// IndexedDB.
///
/// This is useful when (de)serializing an event which is required to be either
/// in-band or out-of-band.
#[derive(Debug, Serialize, Deserialize)]
pub struct GenericEvent<P> {
    /// The full content of the event.
    pub content: TimelineEvent,
    /// The position of the event, if it is in a chunk.
    pub position: P,
}

impl<P> GenericEvent<P> {
    pub fn relation(&self) -> Option<(OwnedEventId, String)> {
        extract_event_relation(self.content.raw())
    }
}

/// A concrete instance of [`GenericEvent`] for in-band events, i.e.,
/// events which are part of a chunk and therefore have a position.
pub type InBandEvent = GenericEvent<Position>;

/// A concrete instance of [`GenericEvent`] for out-of-band events, i.e.,
/// events which are not part of a chunk and therefore have no position.
pub type OutOfBandEvent = GenericEvent<()>;

/// A representation of [`Position`](matrix_sdk_base::linked_chunk::Position)
/// which can be stored in IndexedDB.
#[derive(Debug, Default, Copy, Clone, Serialize, Deserialize)]
pub struct Position {
    /// The identifier of the chunk.
    pub chunk_identifier: u64,
    /// The index of the event within the chunk.
    pub index: usize,
}

impl From<Position> for matrix_sdk_base::linked_chunk::Position {
    fn from(value: Position) -> Self {
        Self::new(ChunkIdentifier::new(value.chunk_identifier), value.index)
    }
}

impl From<matrix_sdk_base::linked_chunk::Position> for Position {
    fn from(value: matrix_sdk_base::linked_chunk::Position) -> Self {
        Self { chunk_identifier: value.chunk_identifier().index(), index: value.index() }
    }
}

/// A representation of [`Gap`](matrix_sdk_base::linked_chunk::Gap)
/// which can be stored in IndexedDB.
#[derive(Debug, Serialize, Deserialize)]
pub struct Gap {
    /// The token to use in the query, extracted from a previous "from" /
    /// "end" field of a `/messages` response.
    pub prev_token: String,
}
