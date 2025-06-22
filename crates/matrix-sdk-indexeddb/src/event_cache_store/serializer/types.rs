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

//! Types used for (de)serialization of event cache store data.
//!
//! These types are wrappers around the types found in
//! [`crate::event_cache_store::types`] and prepare those types for
//! serialization in IndexedDB. They are constructed by extracting
//! relevant values from the inner types, storing those values in indexed
//! fields, and then storing the full types in a possibly encrypted form. This
//! allows the data to be encrypted, while still allowing for efficient querying
//! and retrieval of data.
//!
//! Each top-level type represents an object store in IndexedDB and each
//! field - except the content field - represents an index on that object store.
//! These types mimic the structure of the object stores and indices created in
//! [`crate::event_cache_store::migrations`].

use matrix_sdk_base::linked_chunk::ChunkIdentifier;
use matrix_sdk_crypto::CryptoStoreError;
use ruma::{events::relation::RelationType, owned_event_id, OwnedEventId, RoomId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    event_cache_store::{
        migrations::keys,
        types::{Chunk, Event, Gap, InBandEvent, Position},
    },
    serializer::{IndexeddbSerializer, MaybeEncrypted},
};

pub trait Indexed: Sized {
    const OBJECT_STORE: &'static str;

    type IndexedType;
    type Error;

    fn to_indexed(
        &self,
        room_id: &RoomId,
        serializer: &IndexeddbSerializer,
    ) -> Result<Self::IndexedType, Self::Error>;

    fn from_indexed(
        indexed: Self::IndexedType,
        serializer: &IndexeddbSerializer,
    ) -> Result<Self, Self::Error>;
}

pub trait IndexedKey<T: Indexed> {
    const PATH: &'static str;

    type KeyComponents;

    fn index() -> Option<&'static str> {
        None
    }

    fn encode(
        room_id: &RoomId,
        components: &Self::KeyComponents,
        serializer: &IndexeddbSerializer,
    ) -> Self;
}

const INDEXED_KEY_LOWER_CHARACTER: char = '\u{0000}';
const INDEXED_KEY_UPPER_CHARACTER: char = '\u{FFFF}';

pub trait IndexedKeyBounds<T: Indexed>: IndexedKey<T> + Sized {
    fn lower_key_components() -> Self::KeyComponents;

    fn encode_lower(room_id: &RoomId, serializer: &IndexeddbSerializer) -> Self {
        <Self as IndexedKey<T>>::encode(room_id, &Self::lower_key_components(), serializer)
    }

    fn upper_key_components() -> Self::KeyComponents;

    fn encode_upper(room_id: &RoomId, serializer: &IndexeddbSerializer) -> Self {
        <Self as IndexedKey<T>>::encode(room_id, &Self::upper_key_components(), serializer)
    }
}

pub trait IndexedPartialKeyBounds<T: Indexed, PartialKeyComponents>: IndexedKey<T> {
    fn encode_partial_lower(
        room_id: &RoomId,
        components: &PartialKeyComponents,
        serializer: &IndexeddbSerializer,
    ) -> Self;

    fn encode_partial_upper(
        room_id: &RoomId,
        components: &PartialKeyComponents,
        serializer: &IndexeddbSerializer,
    ) -> Self;
}

/// A type that wraps a (de)serialized value `value` and associates it
/// with an identifier, `id`.
///
/// This is useful for (de)serializing values to/from an object store
/// and ensuring that they are well-formed, as each of the object stores
/// uses `id` as its key path.
#[derive(Debug, Deserialize, Serialize)]
pub struct ValueWithId {
    pub id: String,
    pub value: MaybeEncrypted,
}

/// Represents the [`LINKED_CHUNKS`][1] object store.
///
/// [1]: crate::event_cache_store::migrations::v1::create_linked_chunks_object_store
#[derive(Debug, Serialize, Deserialize)]
pub struct IndexedChunk {
    /// The primary key of the object store.
    pub id: IndexedChunkIdKey,
    /// An indexed key on the object store, which represents the
    /// [`IndexedChunkIdKey`] of the next chunk in the linked list, if it
    /// exists.
    pub next: IndexedNextChunkIdKey,
    /// The (possibly) encrypted content of the chunk.
    pub content: IndexedChunkContent,
}

impl Indexed for Chunk {
    const OBJECT_STORE: &'static str = keys::LINKED_CHUNKS;

    type IndexedType = IndexedChunk;
    type Error = CryptoStoreError;

    fn to_indexed(
        &self,
        room_id: &RoomId,
        serializer: &IndexeddbSerializer,
    ) -> Result<Self::IndexedType, Self::Error> {
        Ok(IndexedChunk {
            id: <IndexedChunkIdKey as IndexedKey<Chunk>>::encode(
                room_id,
                &ChunkIdentifier::new(self.identifier),
                serializer,
            ),
            next: IndexedNextChunkIdKey::encode(
                room_id,
                &self.next.map(ChunkIdentifier::new),
                serializer,
            ),
            content: serializer.maybe_encrypt_value(self)?,
        })
    }

    fn from_indexed(
        indexed: Self::IndexedType,
        serializer: &IndexeddbSerializer,
    ) -> Result<Self, Self::Error> {
        serializer.maybe_decrypt_value(indexed.content)
    }
}

/// The value associated with the [primary key](IndexedChunk::id) of the
/// [`LINKED_CHUNKS`][1] object store, which is constructed from:
///
/// - The (possibly) encrypted Room ID
/// - The Chunk ID.
///
/// [1]: crate::event_cache_store::migrations::v1::create_linked_chunks_object_store
#[derive(Debug, Serialize, Deserialize)]
pub struct IndexedChunkIdKey(IndexedRoomId, IndexedChunkId);

impl IndexedKey<Chunk> for IndexedChunkIdKey {
    const PATH: &'static str = keys::LINKED_CHUNKS_KEY_PATH;

    type KeyComponents = ChunkIdentifier;

    fn encode(
        room_id: &RoomId,
        chunk_id: &ChunkIdentifier,
        serializer: &IndexeddbSerializer,
    ) -> Self {
        let room_id = serializer.encode_key_as_string(keys::LINKED_CHUNKS, room_id);
        let chunk_id = chunk_id.index();
        Self(room_id, chunk_id)
    }
}

impl IndexedKeyBounds<Chunk> for IndexedChunkIdKey {
    fn lower_key_components() -> Self::KeyComponents {
        ChunkIdentifier::new(0)
    }

    fn upper_key_components() -> Self::KeyComponents {
        ChunkIdentifier::new(js_sys::Number::MAX_SAFE_INTEGER as u64)
    }
}

pub type IndexedRoomId = String;
pub type IndexedChunkId = u64;
pub type IndexedChunkContent = MaybeEncrypted;

/// The value associated with the [`next`](IndexedChunk::next) index of the
/// [`LINKED_CHUNKS`][1] object store, which is constructed from:
///
/// - The (possibly) encrypted Room ID
/// - The Chunk ID, if there is a next chunk in the list.
///
/// Note: it would be more convenient to represent this type with an optional
/// Chunk ID, but unfortunately, this creates an issue when querying for objects
/// that don't have a `next` value, because `None` serializes to `null` which
/// is an invalid value in any part of an IndexedDB query.
///
/// Furthermore, each variant must serialize to the same type, so the `None`
/// variant must contain a non-empty tuple.
///
/// [1]: crate::event_cache_store::migrations::v1::create_linked_chunks_object_store
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IndexedNextChunkIdKey {
    /// There is no next chunk.
    None((IndexedRoomId,)),
    /// The identifier of the next chunk in the list.
    Some(IndexedChunkIdKey),
}

impl IndexedKey<Chunk> for IndexedNextChunkIdKey {
    const PATH: &'static str = keys::LINKED_CHUNKS_NEXT_KEY_PATH;

    type KeyComponents = Option<ChunkIdentifier>;

    fn index() -> Option<&'static str> {
        Some(keys::LINKED_CHUNKS_NEXT)
    }

    fn encode(
        room_id: &RoomId,
        next_chunk_id: &Option<ChunkIdentifier>,
        serializer: &IndexeddbSerializer,
    ) -> Self {
        next_chunk_id
            .map(|id| {
                Self::Some(<IndexedChunkIdKey as IndexedKey<Chunk>>::encode(
                    room_id, &id, serializer,
                ))
            })
            .unwrap_or_else(|| {
                let room_id = serializer.encode_key_as_string(keys::LINKED_CHUNKS, room_id);
                Self::None((room_id,))
            })
    }
}

impl IndexedKeyBounds<Chunk> for IndexedNextChunkIdKey {
    fn lower_key_components() -> Self::KeyComponents {
        None
    }
    fn upper_key_components() -> Self::KeyComponents {
        Some(ChunkIdentifier::new(js_sys::Number::MAX_SAFE_INTEGER as u64))
    }
}

/// Represents the [`EVENTS`][1] object store.
///
/// [1]: crate::event_cache_store::migrations::v1::create_events_object_store
#[derive(Debug, Serialize, Deserialize)]
pub struct IndexedEvent {
    /// The primary key of the object store.
    pub id: IndexedEventIdKey,
    /// An indexed key on the object store, which represents the position of the
    /// event, if it is in a chunk.
    pub position: Option<IndexedEventPositionKey>,
    /// An indexed key on the object store, which represents the relationship
    /// between this event and another event, if one exists.
    pub relation: Option<IndexedEventRelationKey>,
    /// The (possibly) encrypted content of the event.
    pub content: IndexedEventContent,
}

#[derive(Debug, Error)]
pub enum IndexedEventError {
    #[error("no event id")]
    NoEventId,
    #[error("crypto store: {0}")]
    CryptoStore(#[from] CryptoStoreError),
}

impl Indexed for Event {
    const OBJECT_STORE: &'static str = keys::EVENTS;

    type IndexedType = IndexedEvent;
    type Error = IndexedEventError;

    fn to_indexed(
        &self,
        room_id: &RoomId,
        serializer: &IndexeddbSerializer,
    ) -> Result<Self::IndexedType, Self::Error> {
        let event_id = self.event_id().ok_or(Self::Error::NoEventId)?;
        let id = IndexedEventIdKey::encode(room_id, &event_id, serializer);
        let position = self
            .position()
            .map(|position| IndexedEventPositionKey::encode(room_id, &position, serializer));
        let relation = self.relation().map(|(related_event, relation_type)| {
            IndexedEventRelationKey::encode(
                room_id,
                &(related_event, RelationType::from(relation_type)),
                serializer,
            )
        });
        Ok(IndexedEvent { id, position, relation, content: serializer.maybe_encrypt_value(self)? })
    }

    fn from_indexed(
        indexed: Self::IndexedType,
        serializer: &IndexeddbSerializer,
    ) -> Result<Self, Self::Error> {
        serializer.maybe_decrypt_value(indexed.content).map_err(Into::into)
    }
}

impl Indexed for InBandEvent {
    const OBJECT_STORE: &'static str = keys::EVENTS;

    type IndexedType = IndexedEvent;
    type Error = IndexedEventError;

    fn to_indexed(
        &self,
        room_id: &RoomId,
        serializer: &IndexeddbSerializer,
    ) -> Result<Self::IndexedType, Self::Error> {
        let event_id = self.content.event_id().ok_or(Self::Error::NoEventId)?;
        let id = IndexedEventIdKey::encode(room_id, &event_id, serializer);
        let position = Some(IndexedEventPositionKey::encode(room_id, &self.position, serializer));
        let relation = self.relation().map(|(related_event, relation_type)| {
            IndexedEventRelationKey::encode(
                room_id,
                &(related_event, RelationType::from(relation_type)),
                serializer,
            )
        });
        Ok(IndexedEvent { id, position, relation, content: serializer.maybe_encrypt_value(self)? })
    }

    fn from_indexed(
        indexed: Self::IndexedType,
        serializer: &IndexeddbSerializer,
    ) -> Result<Self, Self::Error> {
        serializer.maybe_decrypt_value(indexed.content).map_err(Into::into)
    }
}

/// The value associated with the [primary key](IndexedEvent::id) of the
/// [`EVENTS`][1] object store, which is constructed from:
///
/// - The (possibly) encrypted Room ID
/// - The (possibly) encrypted Event ID.
///
/// [1]: crate::event_cache_store::migrations::v1::create_events_object_store
#[derive(Debug, Serialize, Deserialize)]
pub struct IndexedEventIdKey(IndexedRoomId, IndexedEventId);

impl IndexedKey<Event> for IndexedEventIdKey {
    const PATH: &'static str = keys::EVENTS_KEY_PATH;

    type KeyComponents = OwnedEventId;

    fn encode(room_id: &RoomId, event_id: &OwnedEventId, serializer: &IndexeddbSerializer) -> Self {
        let room_id = serializer.encode_key_as_string(keys::ROOMS, room_id);
        let event_id = serializer.encode_key_as_string(keys::EVENTS, event_id);
        IndexedEventIdKey::new(room_id, event_id)
    }
}

impl IndexedKeyBounds<Event> for IndexedEventIdKey {
    fn lower_key_components() -> Self::KeyComponents {
        owned_event_id!("$\u{0000}")
    }

    fn upper_key_components() -> Self::KeyComponents {
        owned_event_id!("$\u{FFFF}")
    }
}

impl IndexedEventIdKey {
    pub fn new(room_id: IndexedRoomId, event_id: IndexedEventId) -> Self {
        Self(room_id, event_id)
    }
}

pub type IndexedEventId = String;

/// The value associated with the [`position`](IndexedEvent::position) index of
/// the [`EVENTS`][1] object store, which is constructed from:
///
/// - The (possibly) encrypted Room ID
/// - The Chunk ID
/// - The index of the event in the chunk.
///
/// [1]: crate::event_cache_store::migrations::v1::create_events_object_store
#[derive(Debug, Serialize, Deserialize)]
pub struct IndexedEventPositionKey(IndexedRoomId, IndexedChunkId, IndexedEventPositionIndex);

impl IndexedKey<Event> for IndexedEventPositionKey {
    const PATH: &'static str = keys::EVENTS_POSITION_KEY_PATH;

    type KeyComponents = Position;

    fn index() -> Option<&'static str> {
        Some(keys::EVENTS_POSITION)
    }

    fn encode(room_id: &RoomId, position: &Position, serializer: &IndexeddbSerializer) -> Self {
        let room_id = serializer.encode_key_as_string(keys::ROOMS, room_id);
        Self(room_id, position.chunk_identifier, position.index)
    }
}

impl IndexedKeyBounds<Event> for IndexedEventPositionKey {
    fn lower_key_components() -> Self::KeyComponents {
        Position { chunk_identifier: 0, index: 0 }
    }

    fn upper_key_components() -> Self::KeyComponents {
        Position {
            chunk_identifier: js_sys::Number::MAX_SAFE_INTEGER as u64,
            index: js_sys::Number::MAX_SAFE_INTEGER as usize,
        }
    }
}

pub type IndexedEventPositionIndex = usize;

/// The value associated with the [`relation`](IndexedEvent::relation) index of
/// the [`EVENTS`][1] object store, which is constructed from:
///
/// - The (possibly) encrypted Room ID
/// - The (possibly) encrypted Event ID of the related event
/// - The type of relationship between the events
///
/// [1]: crate::event_cache_store::migrations::v1::create_events_object_store
#[derive(Debug, Serialize, Deserialize)]
pub struct IndexedEventRelationKey(IndexedRoomId, IndexedEventId, IndexedRelationType);

impl IndexedKey<Event> for IndexedEventRelationKey {
    const PATH: &'static str = keys::EVENTS_RELATION_KEY_PATH;

    type KeyComponents = (OwnedEventId, RelationType);

    fn index() -> Option<&'static str> {
        Some(keys::EVENTS_RELATION)
    }

    fn encode(
        room_id: &RoomId,
        (related_event_id, relation_type): &(OwnedEventId, RelationType),
        serializer: &IndexeddbSerializer,
    ) -> Self {
        let room_id = serializer.encode_key_as_string(keys::ROOMS, room_id);
        let related_event_id =
            serializer.encode_key_as_string(keys::EVENTS_RELATION_RELATED_EVENTS, related_event_id);
        let relation_type = serializer
            .encode_key_as_string(keys::EVENTS_RELATION_RELATION_TYPES, relation_type.to_string());
        Self(room_id, related_event_id, relation_type)
    }
}

impl IndexedKeyBounds<Event> for IndexedEventRelationKey {
    fn lower_key_components() -> Self::KeyComponents {
        (owned_event_id!("$\u{0000}"), RelationType::from(INDEXED_KEY_LOWER_CHARACTER.to_string()))
    }

    fn upper_key_components() -> Self::KeyComponents {
        (owned_event_id!("$\u{0000}"), RelationType::from(INDEXED_KEY_UPPER_CHARACTER.to_string()))
    }
}

impl IndexedPartialKeyBounds<Event, OwnedEventId> for IndexedEventRelationKey {
    fn encode_partial_lower(
        room_id: &RoomId,
        related_event_id: &OwnedEventId,
        serializer: &IndexeddbSerializer,
    ) -> Self {
        let mut key = Self::encode_lower(room_id, serializer);
        key.1 =
            serializer.encode_key_as_string(keys::EVENTS_RELATION_RELATED_EVENTS, related_event_id);
        key
    }

    fn encode_partial_upper(
        room_id: &RoomId,
        related_event_id: &OwnedEventId,
        serializer: &IndexeddbSerializer,
    ) -> Self {
        let mut key = Self::encode_upper(room_id, serializer);
        key.1 =
            serializer.encode_key_as_string(keys::EVENTS_RELATION_RELATED_EVENTS, related_event_id);
        key
    }
}

/// A representation of the relationship between two events (see
/// [`RelationType`](ruma::events::relation::RelationType))
pub type IndexedRelationType = String;

pub type IndexedEventContent = MaybeEncrypted;

/// Represents the [`GAPS`][1] object store.
///
/// [1]: crate::event_cache_store::migrations::v1::create_gaps_object_store
#[derive(Debug, Serialize, Deserialize)]
pub struct IndexedGap {
    /// The primary key of the object store
    pub id: IndexedGapIdKey,
    /// The (possibly) encrypted content of the gap
    pub content: IndexedGapContent,
}

impl Indexed for Gap {
    const OBJECT_STORE: &'static str = keys::GAPS;

    type IndexedType = IndexedGap;
    type Error = CryptoStoreError;

    fn to_indexed(
        &self,
        room_id: &RoomId,
        serializer: &IndexeddbSerializer,
    ) -> Result<Self::IndexedType, Self::Error> {
        Ok(IndexedGap {
            id: <IndexedGapIdKey as IndexedKey<Gap>>::encode(
                room_id,
                &ChunkIdentifier::new(self.chunk_identifier),
                serializer,
            ),
            content: serializer.maybe_encrypt_value(self)?,
        })
    }

    fn from_indexed(
        indexed: Self::IndexedType,
        serializer: &IndexeddbSerializer,
    ) -> Result<Self, Self::Error> {
        serializer.maybe_decrypt_value(indexed.content)
    }
}

/// The primary key of the [`GAPS`][1] object store, which is constructed from:
///
/// - The (possibly) encrypted Room ID
/// - The Chunk ID
///
/// [1]: crate::event_cache_store::migrations::v1::create_gaps_object_store
pub type IndexedGapIdKey = IndexedChunkIdKey;

impl IndexedKey<Gap> for IndexedGapIdKey {
    const PATH: &'static str = keys::GAPS_KEY_PATH;

    type KeyComponents = <IndexedChunkIdKey as IndexedKey<Chunk>>::KeyComponents;

    fn encode(
        room_id: &RoomId,
        components: &Self::KeyComponents,
        serializer: &IndexeddbSerializer,
    ) -> Self {
        <IndexedChunkIdKey as IndexedKey<Chunk>>::encode(room_id, components, serializer)
    }
}

impl IndexedKeyBounds<Gap> for IndexedGapIdKey {
    fn lower_key_components() -> Self::KeyComponents {
        <Self as IndexedKeyBounds<Chunk>>::lower_key_components()
    }

    fn upper_key_components() -> Self::KeyComponents {
        <Self as IndexedKeyBounds<Chunk>>::upper_key_components()
    }
}

pub type IndexedGapContent = MaybeEncrypted;
