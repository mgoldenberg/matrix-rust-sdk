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

mod types;

use gloo_utils::format::JsValueSerdeExt;
use matrix_sdk_base::linked_chunk::ChunkIdentifier;
use ruma::{events::relation::RelationType, EventId, RoomId};
use serde::{de::DeserializeOwned, Serialize};
use wasm_bindgen::JsValue;
use web_sys::IdbKeyRange;

use crate::{
    event_cache_store::{
        keys,
        serializer::types::{
            IndexedChunk, IndexedChunkIdKey, IndexedEvent, IndexedEventIdKey,
            IndexedEventPositionKey, IndexedEventRelationKey, IndexedNextChunkIdKey, ValueWithId,
        },
        types::{Chunk, Event, GenericEvent, InBandEvent, OutOfBandEvent, Position},
    },
    serializer::IndexeddbSerializer,
    IndexeddbEventCacheStoreError,
};

#[derive(Debug)]
pub struct IndexeddbEventCacheStoreSerializer {
    inner: IndexeddbSerializer,
}

impl IndexeddbEventCacheStoreSerializer {
    pub const KEY_SEPARATOR: char = '\u{001D}';
    pub const KEY_LOWER_CHARACTER: char = '\u{0000}';
    pub const KEY_UPPER_CHARACTER: char = '\u{FFFF}';

    pub fn new(inner: IndexeddbSerializer) -> Self {
        Self { inner }
    }

    /// Encodes each tuple in `parts` as a key and then joins them with
    /// `Self::KEY_SEPARATOR`.
    ///
    /// Each tuple is composed of three fields.
    ///
    /// - `&str` - name of an object store
    /// - `&str` - key of an object in the object store
    /// - `bool` - whether to encrypt the fields above in the final key
    ///
    /// Selective encryption is employed to maintain ordering, so that range
    /// queries are possible.
    pub fn encode_key(&self, parts: Vec<(&str, &str, bool)>) -> String {
        let mut end_key = String::new();
        for (i, (table_name, key, should_encrypt)) in parts.into_iter().enumerate() {
            if i > 0 {
                end_key.push(Self::KEY_SEPARATOR);
            }
            let encoded_key = if should_encrypt {
                self.inner.encode_key_as_string(table_name, key)
            } else {
                key.to_owned()
            };
            end_key.push_str(&encoded_key);
        }
        end_key
    }

    pub fn serialize_value(
        &self,
        value: &impl Serialize,
    ) -> Result<JsValue, IndexeddbEventCacheStoreError> {
        self.inner.serialize_value(value).map_err(Into::into)
    }

    /// Serializes `value` and wraps it with a `ValueWithId` using `id`.
    ///
    /// This helps to ensure that values are well-formed before putting them
    /// into an object store, as each of the object stores uses `id` as its key
    /// path.
    pub fn serialize_value_with_id(
        &self,
        id: &str,
        value: &impl Serialize,
    ) -> Result<JsValue, IndexeddbEventCacheStoreError> {
        let serialized = self.inner.maybe_encrypt_value(value)?;
        let res_obj = ValueWithId { id: id.to_owned(), value: serialized };
        Ok(serde_wasm_bindgen::to_value(&res_obj)?)
    }

    /// Deserializes a `value` as a `ValueWithId` and then returns the result of
    /// deserializing the inner `value`.
    ///
    /// The corresponding serialization function, `serialize_value_with_id`
    /// helps to ensure that values are well-formed before putting them into
    /// an object store, as each of the object stores uses `id` as its key
    /// path.
    pub fn deserialize_value_with_id<T: DeserializeOwned>(
        &self,
        value: JsValue,
    ) -> Result<T, IndexeddbEventCacheStoreError> {
        let obj: ValueWithId = value.into_serde()?;
        let deserialized: T = self.inner.maybe_decrypt_value(obj.value)?;
        Ok(deserialized)
    }

    pub fn encode_chunk_id_key(
        &self,
        room_id: &RoomId,
        chunk_id: &ChunkIdentifier,
    ) -> IndexedChunkIdKey {
        let room_id = self.inner.encode_key_as_string(keys::LINKED_CHUNKS, room_id);
        let chunk_id = chunk_id.index();
        IndexedChunkIdKey::new(room_id, chunk_id)
    }

    pub fn encode_chunk_id_key_as_value(
        &self,
        room_id: &RoomId,
        chunk_id: &ChunkIdentifier,
    ) -> Result<JsValue, IndexeddbEventCacheStoreError> {
        Ok(serde_wasm_bindgen::to_value(&self.encode_chunk_id_key(room_id, chunk_id))?)
    }

    pub fn encode_next_chunk_id_key(
        &self,
        room_id: &RoomId,
        next_chunk_id: Option<ChunkIdentifier>,
    ) -> IndexedNextChunkIdKey {
        next_chunk_id
            .map(|id| IndexedNextChunkIdKey::Some(self.encode_chunk_id_key(room_id, &id)))
            .unwrap_or_else(|| {
                let room_id = self.inner.encode_key_as_string(keys::LINKED_CHUNKS, room_id);
                IndexedNextChunkIdKey::None((room_id,))
            })
    }

    pub fn encode_lower_chunk_id_key(&self, room_id: &RoomId) -> IndexedChunkIdKey {
        let room_id = self.inner.encode_key_as_string(keys::LINKED_CHUNKS, room_id);
        IndexedChunkIdKey::new(room_id, 0)
    }

    pub fn encode_upper_chunk_id_key(&self, room_id: &RoomId) -> IndexedChunkIdKey {
        let room_id = self.inner.encode_key_as_string(keys::LINKED_CHUNKS, room_id);
        IndexedChunkIdKey::new(room_id, js_sys::Number::MAX_SAFE_INTEGER as u64)
    }

    pub fn encode_chunk_id_range_for_room(
        &self,
        room_id: &RoomId,
    ) -> Result<IdbKeyRange, IndexeddbEventCacheStoreError> {
        let lower = serde_wasm_bindgen::to_value(&self.encode_lower_chunk_id_key(room_id))?;
        let upper = serde_wasm_bindgen::to_value(&self.encode_upper_chunk_id_key(room_id))?;
        Ok(IdbKeyRange::bound(&lower, &upper).expect("construct key range"))
    }

    pub fn encode_event_position_key(
        &self,
        room_id: &RoomId,
        position: &Position,
    ) -> IndexedEventPositionKey {
        let room_id = self.inner.encode_key_as_string(keys::ROOMS, room_id);
        IndexedEventPositionKey::new(room_id, position.chunk_identifier, position.index)
    }

    pub fn encode_event_position_key_as_value(
        &self,
        room_id: &RoomId,
        position: &Position,
    ) -> Result<JsValue, IndexeddbEventCacheStoreError> {
        serde_wasm_bindgen::to_value(&self.encode_event_position_key(room_id, position))
            .map_err(Into::into)
    }

    pub fn encode_upper_event_position_key_for_chunk(
        &self,
        room_id: &RoomId,
        chunk_id: u64,
    ) -> IndexedEventPositionKey {
        let room_id = self.inner.encode_key_as_string(keys::ROOMS, room_id);
        IndexedEventPositionKey::new(room_id, chunk_id, js_sys::Number::MAX_SAFE_INTEGER as usize)
    }

    pub fn encode_event_position_range_for_chunk(
        &self,
        room_id: &RoomId,
        chunk_id: u64,
    ) -> Result<IdbKeyRange, IndexeddbEventCacheStoreError> {
        use serde_wasm_bindgen::to_value;
        let lower = to_value(&self.encode_event_position_key(
            room_id,
            &Position { chunk_identifier: chunk_id, index: 0 },
        ))?;
        let upper = to_value(&self.encode_upper_event_position_key_for_chunk(room_id, chunk_id))?;
        Ok(IdbKeyRange::bound(&lower, &upper).expect("construct key range"))
    }

    pub fn encode_event_position_range_for_chunk_from(
        &self,
        room_id: &RoomId,
        position: &Position,
    ) -> Result<IdbKeyRange, IndexeddbEventCacheStoreError> {
        use serde_wasm_bindgen::to_value;
        let lower = to_value(&self.encode_event_position_key(room_id, position))?;
        let upper = to_value(
            &self.encode_upper_event_position_key_for_chunk(room_id, position.chunk_identifier),
        )?;
        Ok(IdbKeyRange::bound(&lower, &upper).expect("construct key range"))
    }

    pub fn encode_event_id_key(&self, room_id: &RoomId, event_id: &EventId) -> IndexedEventIdKey {
        let room_id = self.inner.encode_key_as_string(keys::ROOMS, room_id);
        let event_id = self.inner.encode_key_as_string(keys::EVENTS, event_id);
        IndexedEventIdKey::new(room_id, event_id)
    }

    pub fn encode_event_relation_key(
        &self,
        room_id: &RoomId,
        related_event_id: &EventId,
        relation_type: &RelationType,
    ) -> IndexedEventRelationKey {
        let room_id = self.inner.encode_key_as_string(keys::ROOMS, room_id);
        let related_event =
            self.inner.encode_key_as_string(keys::EVENT_RELATED_EVENTS, related_event_id);
        let relation_type =
            self.inner.encode_key_as_string(keys::EVENT_RELATION_TYPES, relation_type.to_string());
        IndexedEventRelationKey::new(room_id, related_event, relation_type)
    }

    pub fn encode_lower_event_relation_key_for_related_event(
        &self,
        room_id: &RoomId,
        related_event_id: &EventId,
    ) -> IndexedEventRelationKey {
        let room_id = self.inner.encode_key_as_string(keys::ROOMS, room_id);
        let related_event_id =
            self.inner.encode_key_as_string(keys::EVENT_RELATED_EVENTS, related_event_id);
        let relation_type = String::from(Self::KEY_LOWER_CHARACTER);
        IndexedEventRelationKey::new(room_id, related_event_id, relation_type)
    }

    pub fn encode_upper_event_relation_key_for_related_event(
        &self,
        room_id: &RoomId,
        related_event_id: &EventId,
    ) -> IndexedEventRelationKey {
        let room_id = self.inner.encode_key_as_string(keys::ROOMS, room_id);
        let related_event_id =
            self.inner.encode_key_as_string(keys::EVENT_RELATED_EVENTS, related_event_id);
        let relation_type = String::from(Self::KEY_UPPER_CHARACTER);
        IndexedEventRelationKey::new(room_id, related_event_id, relation_type)
    }

    pub fn encode_event_relation_range_for_related_event(
        &self,
        room_id: &RoomId,
        related_event_id: &EventId,
    ) -> Result<IdbKeyRange, IndexeddbEventCacheStoreError> {
        use serde_wasm_bindgen::to_value;
        let lower = to_value(
            &self.encode_lower_event_relation_key_for_related_event(room_id, related_event_id),
        )?;
        let upper = to_value(
            &self.encode_upper_event_relation_key_for_related_event(room_id, related_event_id),
        )?;
        Ok(IdbKeyRange::bound(&lower, &upper).expect("construct key range"))
    }

    pub fn serialize_in_band_event(
        &self,
        room_id: &RoomId,
        event: &InBandEvent,
    ) -> Result<JsValue, IndexeddbEventCacheStoreError> {
        let event_id = event.content.event_id().ok_or(IndexeddbEventCacheStoreError::NoEventId)?;
        let id = self.encode_event_id_key(room_id, &event_id);
        let position = self.encode_event_position_key(room_id, &event.position);
        let relation = event.relation().map(|(related_event, relation_type)| {
            self.encode_event_relation_key(
                room_id,
                &related_event,
                &RelationType::from(relation_type),
            )
        });
        Ok(serde_wasm_bindgen::to_value(&IndexedEvent {
            id,
            position: Some(position),
            relation,
            content: self.inner.maybe_encrypt_value(event)?,
        })?)
    }

    pub fn serialize_out_of_band_event(
        &self,
        room_id: &RoomId,
        event: &OutOfBandEvent,
    ) -> Result<JsValue, IndexeddbEventCacheStoreError> {
        let event_id = event.content.event_id().ok_or(IndexeddbEventCacheStoreError::NoEventId)?;
        let id = self.encode_event_id_key(room_id, &event_id);
        let relation = event.relation().map(|(related_event, relation_type)| {
            self.encode_event_relation_key(
                room_id,
                &related_event,
                &RelationType::from(relation_type),
            )
        });
        Ok(serde_wasm_bindgen::to_value(&IndexedEvent {
            id,
            position: None,
            relation,
            content: self.inner.maybe_encrypt_value(event)?,
        })?)
    }

    pub fn serialize_event(
        &self,
        room_id: &RoomId,
        event: &Event,
    ) -> Result<JsValue, IndexeddbEventCacheStoreError> {
        match event {
            Event::InBand(i) => self.serialize_in_band_event(room_id, i),
            Event::OutOfBand(o) => self.serialize_out_of_band_event(room_id, o),
        }
    }

    /// Decode a value that was previously encoded with
    /// [`Self::serialize_value`].
    pub fn deserialize_value<T: DeserializeOwned>(
        &self,
        value: JsValue,
    ) -> Result<T, IndexeddbEventCacheStoreError> {
        self.inner.deserialize_value(value).map_err(Into::into)
    }

    pub fn deserialize_generic_event<P: DeserializeOwned>(
        &self,
        value: JsValue,
    ) -> Result<GenericEvent<P>, IndexeddbEventCacheStoreError> {
        let indexed: IndexedEvent = value.into_serde()?;
        self.inner.maybe_decrypt_value::<GenericEvent<P>>(indexed.content).map_err(Into::into)
    }

    pub fn deserialize_in_band_event(
        &self,
        value: JsValue,
    ) -> Result<InBandEvent, IndexeddbEventCacheStoreError> {
        self.deserialize_generic_event(value)
    }

    pub fn deserialize_event(
        &self,
        value: JsValue,
    ) -> Result<Event, IndexeddbEventCacheStoreError> {
        let indexed: IndexedEvent = value.into_serde()?;
        self.inner.maybe_decrypt_value(indexed.content).map_err(Into::into)
    }

    pub fn serialize_chunk(
        &self,
        room_id: &RoomId,
        chunk: &Chunk,
    ) -> Result<JsValue, IndexeddbEventCacheStoreError> {
        Ok(serde_wasm_bindgen::to_value(&IndexedChunk {
            id: self.encode_chunk_id_key(room_id, &ChunkIdentifier::new(chunk.identifier)),
            next: self.encode_next_chunk_id_key(room_id, chunk.next.map(ChunkIdentifier::new)),
            content: self.inner.maybe_encrypt_value(chunk)?,
        })?)
    }

    pub fn deserialize_chunk(
        &self,
        value: JsValue,
    ) -> Result<Chunk, IndexeddbEventCacheStoreError> {
        let indexed: IndexedChunk = value.into_serde()?;
        self.inner.maybe_decrypt_value(indexed.content).map_err(Into::into)
    }
}
