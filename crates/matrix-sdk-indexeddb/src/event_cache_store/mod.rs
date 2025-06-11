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

mod builder;
mod migrations;
mod serializer;
mod types;

use std::future::IntoFuture;

use async_trait::async_trait;
pub use builder::IndexeddbEventCacheStoreBuilder;
use indexed_db_futures::{prelude::IdbTransaction, IdbDatabase, IdbQuerySource};
use matrix_sdk_base::{
    deserialized_responses::TimelineEvent,
    event_cache::{
        store::{
            media::{IgnoreMediaRetentionPolicy, MediaRetentionPolicy},
            EventCacheStore, EventCacheStoreError, MemoryStore,
        },
        Event, Gap,
    },
    linked_chunk::{
        ChunkContent, ChunkIdentifier, ChunkIdentifierGenerator, LinkedChunkId, Position, RawChunk,
        Update,
    },
    media::MediaRequestParameters,
};
use matrix_sdk_crypto::CryptoStoreError;
use migrations::keys;
use ruma::{
    events::relation::RelationType, EventId, MilliSecondsSinceUnixEpoch, MxcUri, OwnedEventId,
    RoomId,
};
use tracing::{error, trace};
use wasm_bindgen::JsValue;
use web_sys::{IdbCursorDirection, IdbTransactionMode};

use crate::{
    event_cache_store::{
        serializer::IndexeddbEventCacheStoreSerializer,
        types::{ChunkType, InBandEvent, OutOfBandEvent},
    },
    serializer::IndexeddbSerializerError,
};

#[derive(Debug, thiserror::Error)]
pub enum IndexeddbEventCacheStoreError {
    #[error(transparent)]
    Serialization(#[from] IndexeddbSerializerError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("DomException {name} ({code}): {message}")]
    DomException { name: String, message: String, code: u16 },
    #[error("unsupported")]
    Unsupported,
    #[error(transparent)]
    CryptoStoreError(#[from] CryptoStoreError),
    #[error("chunks contain cycle")]
    ChunksContainCycle,
    #[error("chunks contain disjoint lists")]
    ChunksContainDisjointLists,
    #[error("no max chunk id")]
    NoMaxChunkId,
    #[error("duplicate chunk id")]
    DuplicateChunkId,
    #[error("no event id")]
    NoEventId,
    #[error("duplicate event id")]
    DuplicateEventId,
    #[error("duplicate gap id")]
    DuplicateGapId,
    #[error("gap not found")]
    GapNotFound,
    #[error("media store: {0}")]
    MediaStore(#[from] EventCacheStoreError),
}

impl From<web_sys::DomException> for IndexeddbEventCacheStoreError {
    fn from(frm: web_sys::DomException) -> IndexeddbEventCacheStoreError {
        IndexeddbEventCacheStoreError::DomException {
            name: frm.name(),
            message: frm.message(),
            code: frm.code(),
        }
    }
}

impl From<serde_wasm_bindgen::Error> for IndexeddbEventCacheStoreError {
    fn from(e: serde_wasm_bindgen::Error) -> Self {
        IndexeddbEventCacheStoreError::Json(serde::de::Error::custom(e.to_string()))
    }
}

impl From<IndexeddbEventCacheStoreError> for EventCacheStoreError {
    fn from(e: IndexeddbEventCacheStoreError) -> Self {
        match e {
            IndexeddbEventCacheStoreError::Serialization(
                IndexeddbSerializerError::Serialization(e),
            ) => EventCacheStoreError::Serialization(e),
            IndexeddbEventCacheStoreError::MediaStore(e) => e,
            IndexeddbEventCacheStoreError::Json(e) => EventCacheStoreError::Serialization(e),
            _ => EventCacheStoreError::backend(e),
        }
    }
}

type Result<T, E = IndexeddbEventCacheStoreError> = std::result::Result<T, E>;

#[derive(Debug)]
pub struct IndexeddbEventCacheStore {
    inner: IdbDatabase,
    serializer: IndexeddbEventCacheStoreSerializer,
    media_store: MemoryStore,
}

impl IndexeddbEventCacheStore {
    pub const KEY_SEPARATOR: char = '\u{001D}';
    pub const KEY_LOWER_CHARACTER: char = '\u{0000}';
    pub const KEY_UPPER_CHARACTER: char = '\u{FFFF}';

    pub fn builder() -> IndexeddbEventCacheStoreBuilder {
        IndexeddbEventCacheStoreBuilder::new()
    }

    async fn get_all_events_by_position_with_transaction<K: wasm_bindgen::JsCast>(
        &self,
        transaction: &IdbTransaction<'_>,
        key: &K,
    ) -> Result<js_sys::Array, IndexeddbEventCacheStoreError> {
        transaction
            .object_store(keys::EVENTS)?
            .index(keys::EVENTS_POSITION)?
            .get_all_with_key(key)?
            .await
            .map_err(Into::into)
    }

    async fn get_all_events_by_id_with_transaction<K: wasm_bindgen::JsCast>(
        &self,
        transaction: &IdbTransaction<'_>,
        key: &K,
    ) -> Result<Vec<types::Event>, IndexeddbEventCacheStoreError> {
        let mut events = Vec::new();
        let values = transaction.object_store(keys::EVENTS)?.get_all_with_key(key)?.await?;
        for value in values {
            events.push(self.serializer.deserialize_event(value)?);
        }
        Ok(events)
    }

    async fn get_event_by_id_with_transaction(
        &self,
        transaction: &IdbTransaction<'_>,
        room_id: &RoomId,
        event_id: &EventId,
    ) -> Result<Option<types::Event>, IndexeddbEventCacheStoreError> {
        let key =
            serde_wasm_bindgen::to_value(&self.serializer.encode_event_id_key(room_id, event_id))?;
        let mut events = self.get_all_events_by_id_with_transaction(transaction, &key).await?;
        if events.len() > 1 {
            return Err(IndexeddbEventCacheStoreError::DuplicateEventId);
        }
        Ok(events.pop())
    }

    async fn get_event_by_id(
        &self,
        room_id: &RoomId,
        event_id: &EventId,
    ) -> Result<Option<types::Event>, IndexeddbEventCacheStoreError> {
        let transaction =
            self.inner.transaction_on_one_with_mode(keys::EVENTS, IdbTransactionMode::Readonly)?;
        self.get_event_by_id_with_transaction(&transaction, room_id, event_id).await
    }

    async fn get_all_events_by_relation_range_with_transaction<K: wasm_bindgen::JsCast>(
        &self,
        transaction: &IdbTransaction<'_>,
        key: &K,
    ) -> Result<Vec<types::Event>, IndexeddbEventCacheStoreError> {
        let mut events = Vec::new();
        let values = transaction
            .object_store(keys::EVENTS)?
            .index(keys::EVENTS_RELATION)?
            .get_all_with_key(key)?
            .await?;
        for value in values {
            events.push(self.serializer.deserialize_event(value)?);
        }
        Ok(events)
    }

    async fn get_all_events_by_relation_range<K: wasm_bindgen::JsCast>(
        &self,
        key: &K,
    ) -> Result<Vec<types::Event>, IndexeddbEventCacheStoreError> {
        let transaction =
            self.inner.transaction_on_one_with_mode(keys::EVENTS, IdbTransactionMode::Readonly)?;
        self.get_all_events_by_relation_range_with_transaction(&transaction, key).await
    }

    async fn get_all_events_by_relation_with_transaction(
        &self,
        transaction: &IdbTransaction<'_>,
        room_id: &RoomId,
        related_event_id: &EventId,
        relation_type: &RelationType,
    ) -> Result<Vec<types::Event>, IndexeddbEventCacheStoreError> {
        let key = serde_wasm_bindgen::to_value(&self.serializer.encode_event_relation_key(
            room_id,
            related_event_id,
            relation_type,
        ))?;
        self.get_all_events_by_relation_range_with_transaction(transaction, &key).await
    }

    async fn get_all_events_by_related_event(
        &self,
        room_id: &RoomId,
        related_event_id: &EventId,
    ) -> Result<Vec<types::Event>, IndexeddbEventCacheStoreError> {
        let range = self
            .serializer
            .encode_event_relation_range_for_related_event(room_id, related_event_id)?;
        self.get_all_events_by_relation_range(&range).await
    }

    async fn get_all_events_by_chunk_with_transaction(
        &self,
        transaction: &IdbTransaction<'_>,
        room_id: &RoomId,
        chunk_id: u64,
    ) -> Result<Vec<InBandEvent>, IndexeddbEventCacheStoreError> {
        let range =
            self.serializer.encode_event_position_range_for_chunk(room_id.as_ref(), chunk_id)?;
        let values = self.get_all_events_by_position_with_transaction(transaction, &range).await?;
        let mut events = Vec::new();
        for event in values {
            let event: InBandEvent = self.serializer.deserialize_in_band_event(event)?;
            events.push(event);
        }
        Ok(events)
    }

    async fn get_all_timeline_events_by_chunk_with_transaction(
        &self,
        transaction: &IdbTransaction<'_>,
        room_id: &RoomId,
        chunk_id: u64,
    ) -> Result<Vec<TimelineEvent>, IndexeddbEventCacheStoreError> {
        Ok(self
            .get_all_events_by_chunk_with_transaction(transaction, room_id, chunk_id)
            .await?
            .into_iter()
            .map(|event| event.content)
            .collect())
    }

    async fn get_chunk_with_max_id_in_room_with_transaction(
        &self,
        transaction: &IdbTransaction<'_>,
        room_id: &RoomId,
    ) -> Result<Option<types::Chunk>, IndexeddbEventCacheStoreError> {
        let range = self.serializer.encode_chunk_id_range_for_room(room_id)?;
        let direction = IdbCursorDirection::Prev;
        transaction
            .object_store(keys::LINKED_CHUNKS)?
            .open_cursor_with_range_and_direction(&range, direction)?
            .await?
            .map(|cursor| self.serializer.deserialize_chunk(cursor.value()))
            .transpose()
    }

    async fn get_last_chunk_in_room_with_transaction(
        &self,
        transaction: &IdbTransaction<'_>,
        room_id: &RoomId,
    ) -> Result<Option<types::Chunk>, IndexeddbEventCacheStoreError> {
        if self.get_num_chunks_in_room_with_transaction(transaction, room_id).await? == 0 {
            return Ok(None);
        }
        let key =
            serde_wasm_bindgen::to_value(&self.serializer.encode_next_chunk_id_key(room_id, None))?;
        let last_chunks =
            self.get_all_chunks_by_next_chunk_id_with_transaction(transaction, &key).await?;
        if last_chunks.len() > 1 {
            // There are some chunks in the object store, but there is more than
            // one last chunk, which means that we have disjoint lists.
            return Err(IndexeddbEventCacheStoreError::ChunksContainDisjointLists);
        }
        let Some(last_chunk) = last_chunks.first() else {
            // There are chunks in the object store, but there is no last chunk,
            // which means that we have a cycle somewhere.
            return Err(IndexeddbEventCacheStoreError::ChunksContainCycle);
        };
        Ok(Some(*last_chunk))
    }

    async fn get_all_chunks_by_id_with_transaction<K: wasm_bindgen::JsCast>(
        &self,
        transaction: &IdbTransaction<'_>,
        key: &K,
    ) -> Result<Vec<types::Chunk>, IndexeddbEventCacheStoreError> {
        let mut chunks = Vec::new();
        let values = transaction.object_store(keys::LINKED_CHUNKS)?.get_all_with_key(key)?.await?;
        for value in values {
            chunks.push(self.serializer.deserialize_chunk(value)?);
        }
        Ok(chunks)
    }

    async fn get_all_chunks_by_next_chunk_id_with_transaction<K: wasm_bindgen::JsCast>(
        &self,
        transaction: &IdbTransaction<'_>,
        key: &K,
    ) -> Result<Vec<types::Chunk>, IndexeddbEventCacheStoreError> {
        let mut chunks = Vec::new();
        let values = transaction
            .object_store(keys::LINKED_CHUNKS)?
            .index(keys::LINKED_CHUNKS_NEXT)?
            .get_all_with_key(key)?
            .await?;
        for value in values {
            chunks.push(self.serializer.deserialize_chunk(value)?);
        }
        Ok(chunks)
    }

    async fn get_num_chunks_in_room_with_transaction(
        &self,
        transaction: &IdbTransaction<'_>,
        room_id: &RoomId,
    ) -> Result<u32, IndexeddbEventCacheStoreError> {
        let key = self.serializer.encode_chunk_id_range_for_room(room_id)?;
        transaction
            .object_store(keys::LINKED_CHUNKS)?
            .count_with_key(&key)?
            .await
            .map_err(Into::into)
    }

    async fn get_chunk_by_id_with_transaction(
        &self,
        transaction: &IdbTransaction<'_>,
        room_id: &RoomId,
        chunk_id: &ChunkIdentifier,
    ) -> Result<Option<types::Chunk>, IndexeddbEventCacheStoreError> {
        let key =
            serde_wasm_bindgen::to_value(&self.serializer.encode_chunk_id_key(room_id, chunk_id))?;
        let mut chunks = self.get_all_chunks_by_id_with_transaction(transaction, &key).await?;
        if chunks.len() > 1 {
            return Err(IndexeddbEventCacheStoreError::DuplicateChunkId);
        }
        Ok(chunks.pop())
    }

    async fn get_all_chunks_in_room_with_transaction(
        &self,
        transaction: &IdbTransaction<'_>,
        room_id: &RoomId,
    ) -> Result<Vec<types::Chunk>, IndexeddbEventCacheStoreError> {
        self.get_all_chunks_by_id_with_transaction(
            transaction,
            &self.serializer.encode_chunk_id_range_for_room(room_id)?,
        )
        .await
    }

    async fn add_chunk_with_transaction(
        &self,
        transaction: &IdbTransaction<'_>,
        room_id: &RoomId,
        chunk: &types::Chunk,
    ) -> Result<(), IndexeddbEventCacheStoreError> {
        let store = transaction.object_store(keys::LINKED_CHUNKS)?;

        store.add_val_owned(self.serializer.serialize_chunk(room_id, chunk)?)?;

        if let Some(previous) = chunk.previous {
            let previous_identifier = ChunkIdentifier::new(previous);
            if let Some(mut previous_chunk) = self
                .get_chunk_by_id_with_transaction(transaction, room_id, &previous_identifier)
                .await?
            {
                previous_chunk.next = Some(chunk.identifier);
                store.put_val_owned(self.serializer.serialize_chunk(room_id, &previous_chunk)?)?;
            }
        }

        if let Some(next) = chunk.next {
            let next_identifier = ChunkIdentifier::new(next);
            if let Some(mut next_chunk) = self
                .get_chunk_by_id_with_transaction(transaction, room_id, &next_identifier)
                .await?
            {
                next_chunk.previous = Some(chunk.identifier);
                store.put_val_owned(self.serializer.serialize_chunk(room_id, &next_chunk)?)?;
            }
        }

        Ok(())
    }

    async fn delete_chunk_by_id_with_transaction(
        &self,
        transaction: &IdbTransaction<'_>,
        room_id: &RoomId,
        id: &ChunkIdentifier,
    ) -> Result<(), IndexeddbEventCacheStoreError> {
        let store = transaction.object_store(keys::LINKED_CHUNKS)?;

        let option = self.get_chunk_by_id_with_transaction(transaction, room_id, id).await?;
        if let Some(chunk) = option {
            if let Some(previous) = chunk.previous {
                let previous_identifier = ChunkIdentifier::new(previous);
                if let Some(mut previous_chunk) = self
                    .get_chunk_by_id_with_transaction(transaction, room_id, &previous_identifier)
                    .await?
                {
                    previous_chunk.next = chunk.next;
                    store.put_val_owned(
                        self.serializer.serialize_chunk(room_id, &previous_chunk)?,
                    )?;
                }
            }
            if let Some(next) = chunk.next {
                let next_identifier = ChunkIdentifier::new(next);
                if let Some(mut next_chunk) = self
                    .get_chunk_by_id_with_transaction(transaction, room_id, &next_identifier)
                    .await?
                {
                    next_chunk.previous = chunk.previous;
                    store.put_val_owned(self.serializer.serialize_chunk(room_id, &next_chunk)?)?;
                }
            }
            store.delete_owned(self.serializer.encode_chunk_id_key_as_value(room_id, id)?)?;
        }
        Ok(())
    }

    async fn get_all_gaps_by_id_with_transaction<K: wasm_bindgen::JsCast>(
        &self,
        transaction: &IdbTransaction<'_>,
        key: &K,
    ) -> Result<Vec<types::Gap>, IndexeddbEventCacheStoreError> {
        let mut gaps = Vec::new();
        let values = transaction.object_store(keys::GAPS)?.get_all_with_key(key)?.await?;
        for value in values {
            gaps.push(self.serializer.deserialize_gap(value)?);
        }
        Ok(gaps)
    }

    async fn get_gap_by_id_with_transaction(
        &self,
        transaction: &IdbTransaction<'_>,
        room_id: &RoomId,
        chunk_id: &ChunkIdentifier,
    ) -> Result<Option<types::Gap>, IndexeddbEventCacheStoreError> {
        let key =
            serde_wasm_bindgen::to_value(&self.serializer.encode_gap_id_key(room_id, chunk_id))?;
        let mut gaps = self.get_all_gaps_by_id_with_transaction(transaction, &key).await?;
        if gaps.len() > 1 {
            return Err(IndexeddbEventCacheStoreError::DuplicateGapId);
        }
        Ok(gaps.pop())
    }

    async fn add_gap_with_transaction(
        &self,
        transaction: &IdbTransaction<'_>,
        room_id: &RoomId,
        chunk_id: &ChunkIdentifier,
        gap: &types::Gap,
    ) -> Result<(), IndexeddbEventCacheStoreError> {
        transaction
            .object_store(keys::GAPS)?
            .add_val_owned(self.serializer.serialize_gap(room_id, chunk_id, gap)?)?
            .await?;
        Ok(())
    }
}

// Small hack to have the following macro invocation act as the appropriate
// trait impl block on wasm, but still be compiled on non-wasm as a regular
// impl block otherwise.
//
// The trait impl doesn't compile on non-wasm due to unfulfilled trait bounds,
// this hack allows us to still have most of rust-analyzer's IDE functionality
// within the impl block without having to set it up to check things against
// the wasm target (which would disable many other parts of the codebase).
#[cfg(target_arch = "wasm32")]
macro_rules! impl_event_cache_store {
    ( $($body:tt)* ) => {
        #[async_trait(?Send)]
        impl EventCacheStore for IndexeddbEventCacheStore {
            type Error = IndexeddbEventCacheStoreError;

            $($body)*
        }
    };
}

#[cfg(not(target_arch = "wasm32"))]
macro_rules! impl_event_cache_store {
    ( $($body:tt)* ) => {
        impl IndexeddbEventCacheStore {
            $($body)*
        }
    };
}

impl_event_cache_store! {
    /// Try to take a lock using the given store.
    async fn try_take_leased_lock(
        &self,
        lease_duration_ms: u32,
        key: &str,
        holder: &str,
    ) -> Result<bool, IndexeddbEventCacheStoreError> {
        let key = JsValue::from_str(key);
        let txn =
            self.inner.transaction_on_one_with_mode(keys::CORE, IdbTransactionMode::Readwrite)?;
        let object_store = txn.object_store(keys::CORE)?;

        #[derive(serde::Deserialize, serde::Serialize)]
        struct Lease {
            holder: String,
            expiration_ts: u64,
        }

        let now_ts: u64 = MilliSecondsSinceUnixEpoch::now().get().into();
        let expiration_ts = now_ts + lease_duration_ms as u64;
        let value =
            self.serializer.serialize_value(&Lease { holder: holder.to_owned(), expiration_ts })?;

        let prev = object_store.get(&key)?.await?;
        match prev {
            Some(prev) => {
                let lease: Lease = self.serializer.deserialize_value(prev)?;
                if lease.holder == holder || lease.expiration_ts < now_ts {
                    object_store.put_key_val(&key, &value)?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            None => {
                object_store.put_key_val(&key, &value)?;
                Ok(true)
            }
        }
    }

    /// An [`Update`] reflects an operation that has happened inside a linked
    /// chunk. The linked chunk is used by the event cache to store the events
    /// in-memory. This method aims at forwarding this update inside this store.
    async fn handle_linked_chunk_updates(
        &self,
        linked_chunk_id: LinkedChunkId<'_>,
        updates: Vec<Update<Event, Gap>>,
    ) -> Result<(), IndexeddbEventCacheStoreError> {
        let room_id = linked_chunk_id.room_id();

        let tx = self.inner.transaction_on_multi_with_mode(
            &[keys::LINKED_CHUNKS, keys::GAPS, keys::EVENTS],
            IdbTransactionMode::Readwrite,
        )?;

        let linked_chunks = tx.object_store(keys::LINKED_CHUNKS)?;
        let events = tx.object_store(keys::EVENTS)?;
        let event_positions = events.index(keys::EVENTS_POSITION)?;

        for update in updates {
            match update {
                Update::NewItemsChunk { previous, new, next } => {
                    trace!(%room_id, "Inserting new chunk (prev={previous:?}, new={new:?}, next={next:?})");
                    self.add_chunk_with_transaction(
                        &tx,
                        room_id,
                        &types::Chunk {
                            identifier: new.index(),
                            previous: previous.map(|i| i.index()),
                            next: next.map(|i| i.index()),
                            chunk_type: ChunkType::Event,
                        },
                    )
                    .await?;
                }
                Update::NewGapChunk { previous, new, next, gap } => {
                    trace!(%room_id, "Inserting new gap (prev={previous:?}, new={new:?}, next={next:?})");
                    self.add_gap_with_transaction(
                        &tx,
                        room_id,
                        &new,
                        &types::Gap { prev_token: gap.prev_token },
                    )
                    .await?;
                    self.add_chunk_with_transaction(
                        &tx,
                        room_id,
                        &types::Chunk {
                            identifier: new.index(),
                            previous: previous.map(|i| i.index()),
                            next: next.map(|i| i.index()),
                            chunk_type: ChunkType::Gap,
                        },
                    )
                    .await?;
                }
                Update::RemoveChunk(id) => {
                    trace!("Removing chunk {id:?}");
                    self.delete_chunk_by_id_with_transaction(&tx, room_id, &id).await?;
                }
                Update::PushItems { at, items } => {
                    let chunk_identifier = at.chunk_identifier().index();

                    trace!(%room_id, "pushing {} items @ {chunk_identifier}", items.len());

                    for (i, item) in items.into_iter().enumerate() {
                        let value = self.serializer.serialize_in_band_event(
                            room_id,
                            &InBandEvent {
                                content: item,
                                position: types::Position {
                                    chunk_identifier,
                                    index: at.index() + i,
                                },
                            },
                        )?;
                        events.put_val(&value)?.into_future().await?;
                    }
                }
                Update::ReplaceItem { at, item } => {
                    let chunk_id = at.chunk_identifier().index();
                    let index = at.index();

                    trace!(%room_id, "replacing item @ {chunk_id}:{index}");

                    // First remove the event in the given position, if it exists
                    let key = self
                        .serializer
                        .encode_event_position_key_as_value(room_id.as_ref(), &at.into())?;
                    if let Some(cursor) = event_positions.open_cursor_with_range(&key)?.await? {
                        cursor.delete()?.await?;
                    }

                    // Then put the new event in the given position
                    let value = self.serializer.serialize_in_band_event(
                        room_id,
                        &InBandEvent { content: item, position: at.into() },
                    )?;
                    events.put_val(&value)?.await?;
                }
                Update::RemoveItem { at } => {
                    let chunk_id = at.chunk_identifier().index();
                    let index = at.index();

                    trace!(%room_id, "removing item @ {chunk_id}:{index}");

                    let key = self
                        .serializer
                        .encode_event_position_key_as_value(room_id.as_ref(), &at.into())?;
                    if let Some(cursor) = event_positions.open_cursor_with_range(&key)?.await? {
                        cursor.delete()?.await?;
                    }
                }
                Update::DetachLastItems { at } => {
                    let chunk_id = at.chunk_identifier().index();
                    let index = at.index();

                    trace!(%room_id, "detaching last items @ {chunk_id}:{index}");

                    let key_range = self
                        .serializer
                        .encode_event_position_range_for_chunk_from(room_id.as_ref(), &at.into())?;
                    if let Some(cursor) =
                        event_positions.open_cursor_with_range(&key_range)?.await?
                    {
                        while cursor.key().is_some() {
                            let event: InBandEvent =
                                self.serializer.deserialize_in_band_event(cursor.value())?;
                            if event.position.index >= index {
                                cursor.delete()?.await?;
                            }
                            cursor.continue_cursor()?.await?;
                        }
                    }
                }
                Update::StartReattachItems | Update::EndReattachItems => {
                    // Nothing? See sqlite implementation
                }
                Update::Clear => {
                    trace!(%room_id, "clearing all events");
                    let chunks = self.get_all_chunks_in_room_with_transaction(&tx, room_id).await?;
                    for chunk in chunks {
                        // Delete all events for this chunk
                        let events_key_range =
                            self.serializer.encode_event_position_range_for_chunk(
                                room_id.as_ref(),
                                chunk.identifier,
                            )?;
                        events.delete_owned(events_key_range)?;
                        linked_chunks.delete_owned(
                            self.serializer.encode_chunk_id_key_as_value(
                                room_id,
                                &ChunkIdentifier::new(chunk.identifier),
                            )?,
                        )?;
                    }
                }
            }
        }

        tx.await.into_result()?;
        Ok(())
    }

    /// Return all the raw components of a linked chunk, so the caller may
    /// reconstruct the linked chunk later.
    #[doc(hidden)]
    async fn load_all_chunks(
        &self,
        linked_chunk_id: LinkedChunkId<'_>,
    ) -> Result<Vec<RawChunk<Event, Gap>>, IndexeddbEventCacheStoreError> {
        let room_id = linked_chunk_id.room_id();
        let transaction = self.inner.transaction_on_multi_with_mode(
            &[keys::LINKED_CHUNKS, keys::GAPS, keys::EVENTS],
            IdbTransactionMode::Readwrite,
        )?;

        let mut result = Vec::new();
        let chunks = self.get_all_chunks_in_room_with_transaction(&transaction, room_id).await?;
        for chunk in chunks {
            let content = match chunk.chunk_type {
                ChunkType::Event => {
                    let events = self
                        .get_all_timeline_events_by_chunk_with_transaction(
                            &transaction,
                            room_id,
                            chunk.identifier,
                        )
                        .await?;
                    ChunkContent::Items(events)
                }
                ChunkType::Gap => {
                    let gap = self
                        .get_gap_by_id_with_transaction(
                            &transaction,
                            room_id,
                            &ChunkIdentifier::new(chunk.identifier),
                        )
                        .await?
                        .ok_or(IndexeddbEventCacheStoreError::GapNotFound)?;
                    ChunkContent::Gap(Gap { prev_token: gap.prev_token })
                }
            };
            let raw_chunk = RawChunk {
                identifier: ChunkIdentifier::new(chunk.identifier),
                content,
                previous: chunk.previous.map(ChunkIdentifier::new),
                next: chunk.next.map(ChunkIdentifier::new),
            };
            result.push(raw_chunk);
        }
        Ok(result)
    }

    /// Load the last chunk of the `LinkedChunk` holding all events of the room
    /// identified by `room_id`.
    ///
    /// This is used to iteratively load events for the `EventCache`.
    async fn load_last_chunk(
        &self,
        linked_chunk_id: LinkedChunkId<'_>,
    ) -> Result<
        (Option<RawChunk<Event, Gap>>, ChunkIdentifierGenerator),
        IndexeddbEventCacheStoreError,
    > {
        let room_id = linked_chunk_id.room_id();
        let transaction = self.inner.transaction_on_multi_with_mode(
            &[keys::LINKED_CHUNKS, keys::EVENTS, keys::GAPS],
            IdbTransactionMode::Readonly,
        )?;
        match self.get_last_chunk_in_room_with_transaction(&transaction, room_id).await? {
            None => Ok((None, ChunkIdentifierGenerator::new_from_scratch())),
            Some(last_chunk) => {
                let content = match last_chunk.chunk_type {
                    ChunkType::Event => {
                        let events = self
                            .get_all_timeline_events_by_chunk_with_transaction(
                                &transaction,
                                room_id.as_ref(),
                                last_chunk.identifier,
                            )
                            .await?;
                        ChunkContent::Items(events)
                    }
                    ChunkType::Gap => {
                        let gap = self
                            .get_gap_by_id_with_transaction(
                                &transaction,
                                room_id,
                                &ChunkIdentifier::new(last_chunk.identifier),
                            )
                            .await?
                            .ok_or(IndexeddbEventCacheStoreError::GapNotFound)?;
                        ChunkContent::Gap(Gap { prev_token: gap.prev_token })
                    }
                };
                let raw_chunk = RawChunk {
                    identifier: ChunkIdentifier::new(last_chunk.identifier),
                    content,
                    previous: last_chunk.previous.map(ChunkIdentifier::new),
                    next: None,
                };
                let max_chunk_id = self
                    .get_chunk_with_max_id_in_room_with_transaction(&transaction, room_id)
                    .await?
                    .map(|chunk| ChunkIdentifier::new(chunk.identifier))
                    .ok_or(IndexeddbEventCacheStoreError::NoMaxChunkId)?;
                let generator =
                    ChunkIdentifierGenerator::new_from_previous_chunk_identifier(max_chunk_id);
                Ok((Some(raw_chunk), generator))
            }
        }
    }

    /// Load the chunk before the chunk identified by `before_chunk_identifier`
    /// of the `LinkedChunk` holding all events of the room identified by
    /// `room_id`
    ///
    /// This is used to iteratively load events for the `EventCache`.
    async fn load_previous_chunk(
        &self,
        linked_chunk_id: LinkedChunkId<'_>,
        before_chunk_identifier: ChunkIdentifier,
    ) -> Result<Option<RawChunk<Event, Gap>>, IndexeddbEventCacheStoreError> {
        let room_id = linked_chunk_id.room_id();
        let transaction = self.inner.transaction_on_multi_with_mode(
            &[keys::LINKED_CHUNKS, keys::EVENTS, keys::GAPS],
            IdbTransactionMode::Readonly,
        )?;
        if let Some(chunk) = self
            .get_chunk_by_id_with_transaction(&transaction, room_id, &before_chunk_identifier)
            .await?
        {
            if let Some(previous_identifier) = chunk.previous {
                if let Some(previous_chunk) = self
                    .get_chunk_by_id_with_transaction(
                        &transaction,
                        room_id,
                        &ChunkIdentifier::new(previous_identifier),
                    )
                    .await?
                {
                    let content = match previous_chunk.chunk_type {
                        ChunkType::Event => {
                            let events = self
                                .get_all_timeline_events_by_chunk_with_transaction(
                                    &transaction,
                                    room_id.as_ref(),
                                    previous_chunk.identifier,
                                )
                                .await?;
                            ChunkContent::Items(events)
                        }
                        ChunkType::Gap => {
                            let gap = self
                                .get_gap_by_id_with_transaction(
                                    &transaction,
                                    room_id,
                                    &ChunkIdentifier::new(previous_chunk.identifier),
                                )
                                .await?
                                .ok_or(IndexeddbEventCacheStoreError::GapNotFound)?;
                            ChunkContent::Gap(Gap { prev_token: gap.prev_token })
                        }
                    };
                    return Ok(Some(RawChunk {
                        content,
                        identifier: ChunkIdentifier::new(previous_chunk.identifier),
                        previous: previous_chunk.previous.map(ChunkIdentifier::new),
                        // TODO: This should always be `before_chunk_identifier`, and if it's not,
                        // something is wrong with our query... should this
                        // be an expect()? Or, at least an error?
                        next: previous_chunk.next.map(ChunkIdentifier::new),
                    }));
                }
            }
        }
        Ok(None)
    }

    /// Clear persisted events for all the rooms.
    ///
    /// This will empty and remove all the linked chunks stored previously,
    /// using the above [`Self::handle_linked_chunk_updates`] methods. It
    /// must *also* delete all the events' content, if they were stored in a
    /// separate table.
    ///
    /// ⚠ This is meant only for super specific use cases, where there shouldn't
    /// be any live in-memory linked chunks. In general, prefer using
    /// `EventCache::clear_all_rooms()` from the common SDK crate.
    async fn clear_all_linked_chunks(&self) -> Result<(), IndexeddbEventCacheStoreError> {
        let tx = self.inner.transaction_on_multi_with_mode(
            &[keys::LINKED_CHUNKS, keys::EVENTS, keys::GAPS],
            IdbTransactionMode::Readwrite,
        )?;

        let object_store = tx.object_store(keys::LINKED_CHUNKS)?;
        object_store.clear()?.await?;

        let object_store = tx.object_store(keys::EVENTS)?;
        object_store.clear()?.await?;

        let object_store = tx.object_store(keys::GAPS)?;
        object_store.clear()?.await?;

        tx.await.into_result()?;
        Ok(())
    }

    /// Given a set of event IDs, return the duplicated events along with their
    /// position if there are any.
    async fn filter_duplicated_events(
        &self,
        linked_chunk_id: LinkedChunkId<'_>,
        events: Vec<OwnedEventId>,
    ) -> Result<Vec<(OwnedEventId, Position)>, IndexeddbEventCacheStoreError> {
        if events.is_empty() {
            return Ok(Vec::new());
        }

        let room_id = linked_chunk_id.room_id();
        let transaction =
            self.inner.transaction_on_one_with_mode(keys::EVENTS, IdbTransactionMode::Readonly)?;

        let mut duplicated = Vec::new();
        for event_id in events {
            if let Some(types::Event::InBand(event)) =
                self.get_event_by_id_with_transaction(&transaction, room_id, &event_id).await?
            {
                duplicated.push((event_id, event.position.into()));
            }
        }
        Ok(duplicated)
    }

    /// Find an event by its ID.
    async fn find_event(
        &self,
        room_id: &RoomId,
        event_id: &EventId,
    ) -> Result<Option<Event>, IndexeddbEventCacheStoreError> {
        self.get_event_by_id(room_id, event_id).await.map(|ok| ok.map(|some| some.take_content()))
    }

    /// Find all the events that relate to a given event.
    ///
    /// An additional filter can be provided to only retrieve related events for
    /// a certain relationship.
    async fn find_event_relations(
        &self,
        room_id: &RoomId,
        event_id: &EventId,
        filter: Option<&[RelationType]>,
    ) -> Result<Vec<Event>, IndexeddbEventCacheStoreError> {
        let mut events = Vec::new();
        match filter {
            Some(relation_types) if !relation_types.is_empty() => {
                let transaction = self
                    .inner
                    .transaction_on_one_with_mode(keys::EVENTS, IdbTransactionMode::Readonly)?;
                for relation_type in relation_types {
                    let result = self
                        .get_all_events_by_relation_with_transaction(
                            &transaction,
                            room_id,
                            event_id,
                            relation_type,
                        )
                        .await?;
                    for event in result {
                        events.push(event.take_content());
                    }
                }
            }
            _ => {
                for event in self.get_all_events_by_related_event(room_id, event_id).await? {
                    events.push(event.take_content());
                }
            }
        };
        Ok(events)
    }

    /// Save an event, that might or might not be part of an existing linked
    /// chunk.
    ///
    /// If the event has no event id, it will not be saved, and the function
    /// must return an Ok result early.
    ///
    /// If the event was already stored with the same id, it must be replaced,
    /// without causing an error.
    async fn save_event(
        &self,
        room_id: &RoomId,
        event: Event,
    ) -> Result<(), IndexeddbEventCacheStoreError> {
        let Some(event_id) = event.event_id() else {
            error!(%room_id, "Trying to save an event with no ID");
            return Ok(());
        };

        let transaction = self
            .inner
            .transaction_on_multi_with_mode(&[keys::EVENTS], IdbTransactionMode::Readwrite)?;

        let event =
            match self.get_event_by_id_with_transaction(&transaction, room_id, &event_id).await? {
                Some(mut inner) => {
                    let _ = inner.replace_content(event);
                    inner
                }
                None => types::Event::OutOfBand(OutOfBandEvent { content: event, position: () }),
            };
        let value = self.serializer.serialize_event(room_id, &event)?;
        transaction.object_store(keys::EVENTS)?.put_val_owned(value)?;
        Ok(())
    }

    /// Add a media file's content in the media store.
    ///
    /// # Arguments
    ///
    /// * `request` - The `MediaRequest` of the file.
    ///
    /// * `content` - The content of the file.
    async fn add_media_content(
        &self,
        request: &MediaRequestParameters,
        content: Vec<u8>,
        ignore_policy: IgnoreMediaRetentionPolicy,
    ) -> Result<(), IndexeddbEventCacheStoreError> {
        self.media_store
            .add_media_content(request, content, ignore_policy)
            .await
            .map_err(Into::into)
    }

    /// Replaces the given media's content key with another one.
    ///
    /// This should be used whenever a temporary (local) MXID has been used, and
    /// it must now be replaced with its actual remote counterpart (after
    /// uploading some content, or creating an empty MXC URI).
    ///
    /// ⚠ No check is performed to ensure that the media formats are consistent,
    /// i.e. it's possible to update with a thumbnail key a media that was
    /// keyed as a file before. The caller is responsible of ensuring that
    /// the replacement makes sense, according to their use case.
    ///
    /// This should not raise an error when the `from` parameter points to an
    /// unknown media, and it should silently continue in this case.
    ///
    /// # Arguments
    ///
    /// * `from` - The previous `MediaRequest` of the file.
    ///
    /// * `to` - The new `MediaRequest` of the file.
    async fn replace_media_key(
        &self,
        from: &MediaRequestParameters,
        to: &MediaRequestParameters,
    ) -> Result<(), IndexeddbEventCacheStoreError> {
        self.media_store.replace_media_key(from, to).await.map_err(Into::into)
    }

    /// Get a media file's content out of the media store.
    ///
    /// # Arguments
    ///
    /// * `request` - The `MediaRequest` of the file.
    async fn get_media_content(
        &self,
        request: &MediaRequestParameters,
    ) -> Result<Option<Vec<u8>>, IndexeddbEventCacheStoreError> {
        self.media_store.get_media_content(request).await.map_err(Into::into)
    }

    /// Remove a media file's content from the media store.
    ///
    /// # Arguments
    ///
    /// * `request` - The `MediaRequest` of the file.
    async fn remove_media_content(
        &self,
        request: &MediaRequestParameters,
    ) -> Result<(), IndexeddbEventCacheStoreError> {
        self.media_store.remove_media_content(request).await.map_err(Into::into)
    }

    /// Get a media file's content associated to an `MxcUri` from the
    /// media store.
    ///
    /// In theory, there could be several files stored using the same URI and a
    /// different `MediaFormat`. This API is meant to be used with a media file
    /// that has only been stored with a single format.
    ///
    /// If there are several media files for a given URI in different formats,
    /// this API will only return one of them. Which one is left as an
    /// implementation detail.
    ///
    /// # Arguments
    ///
    /// * `uri` - The `MxcUri` of the media file.
    async fn get_media_content_for_uri(
        &self,
        uri: &MxcUri,
    ) -> Result<Option<Vec<u8>>, IndexeddbEventCacheStoreError> {
        self.media_store.get_media_content_for_uri(uri).await.map_err(Into::into)
    }

    /// Remove all the media files' content associated to an `MxcUri` from the
    /// media store.
    ///
    /// This should not raise an error when the `uri` parameter points to an
    /// unknown media, and it should return an Ok result in this case.
    ///
    /// # Arguments
    ///
    /// * `uri` - The `MxcUri` of the media files.
    async fn remove_media_content_for_uri(
        &self,
        uri: &MxcUri,
    ) -> Result<(), IndexeddbEventCacheStoreError> {
        self.media_store.remove_media_content_for_uri(uri).await.map_err(Into::into)
    }

    /// Set the `MediaRetentionPolicy` to use for deciding whether to store or
    /// keep media content.
    ///
    /// # Arguments
    ///
    /// * `policy` - The `MediaRetentionPolicy` to use.
    async fn set_media_retention_policy(
        &self,
        policy: MediaRetentionPolicy,
    ) -> Result<(), IndexeddbEventCacheStoreError> {
        self.media_store.set_media_retention_policy(policy).await.map_err(Into::into)
    }

    /// Get the current `MediaRetentionPolicy`.
    fn media_retention_policy(&self) -> MediaRetentionPolicy {
        self.media_store.media_retention_policy()
    }

    /// Set whether the current [`MediaRetentionPolicy`] should be ignored for
    /// the media.
    ///
    /// The change will be taken into account in the next cleanup.
    ///
    /// # Arguments
    ///
    /// * `request` - The `MediaRequestParameters` of the file.
    ///
    /// * `ignore_policy` - Whether the current `MediaRetentionPolicy` should be
    ///   ignored.
    async fn set_ignore_media_retention_policy(
        &self,
        request: &MediaRequestParameters,
        ignore_policy: IgnoreMediaRetentionPolicy,
    ) -> Result<(), IndexeddbEventCacheStoreError> {
        self.media_store
            .set_ignore_media_retention_policy(request, ignore_policy)
            .await
            .map_err(Into::into)
    }

    /// Clean up the media cache with the current `MediaRetentionPolicy`.
    ///
    /// If there is already an ongoing cleanup, this is a noop.
    async fn clean_up_media_cache(&self) -> Result<(), IndexeddbEventCacheStoreError> {
        self.media_store.clean_up_media_cache().await.map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use matrix_sdk_base::{
        event_cache::{
            store::{
                integration_tests::{check_test_event, make_test_event},
                EventCacheStore,
            },
            Gap,
        },
        linked_chunk::{ChunkContent, ChunkIdentifier, Position, Update},
    };
    use matrix_sdk_test::DEFAULT_TEST_ROOM_ID;
    use ruma::room_id;

    use super::*;

    async fn test_linked_chunk_new_items_chunk(store: IndexeddbEventCacheStore) {
        let linked_chunk_id = LinkedChunkId::Room(&DEFAULT_TEST_ROOM_ID);
        let updates = vec![
            Update::NewItemsChunk {
                previous: None,
                new: ChunkIdentifier::new(42),
                next: None, // Note: the store must link the next entry itself.
            },
            Update::NewItemsChunk {
                previous: Some(ChunkIdentifier::new(42)),
                new: ChunkIdentifier::new(13),
                next: Some(ChunkIdentifier::new(37)), /* But it's fine to explicitly pass
                                                       * the next link ahead of time. */
            },
            Update::NewItemsChunk {
                previous: Some(ChunkIdentifier::new(13)),
                new: ChunkIdentifier::new(37),
                next: None,
            },
        ];
        store.handle_linked_chunk_updates(linked_chunk_id, updates).await.unwrap();

        let mut chunks = store.load_all_chunks(linked_chunk_id).await.unwrap();
        assert_eq!(chunks.len(), 3);

        // Chunks are ordered from smaller to bigger IDs.
        let c = chunks.remove(0);
        assert_eq!(c.identifier, ChunkIdentifier::new(13));
        assert_eq!(c.previous, Some(ChunkIdentifier::new(42)));
        assert_eq!(c.next, Some(ChunkIdentifier::new(37)));
        assert_matches!(c.content, ChunkContent::Items(events) => {
            assert!(events.is_empty());
        });

        let c = chunks.remove(0);
        assert_eq!(c.identifier, ChunkIdentifier::new(37));
        assert_eq!(c.previous, Some(ChunkIdentifier::new(13)));
        assert_eq!(c.next, None);
        assert_matches!(c.content, ChunkContent::Items(events) => {
            assert!(events.is_empty());
        });

        let c = chunks.remove(0);
        assert_eq!(c.identifier, ChunkIdentifier::new(42));
        assert_eq!(c.previous, None);
        assert_eq!(c.next, Some(ChunkIdentifier::new(13)));
        assert_matches!(c.content, ChunkContent::Items(events) => {
            assert!(events.is_empty());
        });
    }

    async fn test_add_gap_chunk_and_delete_it_immediately(store: IndexeddbEventCacheStore) {
        let linked_chunk_id = LinkedChunkId::Room(&DEFAULT_TEST_ROOM_ID);
        let updates = vec![Update::NewGapChunk {
            previous: None,
            new: ChunkIdentifier::new(1),
            next: None,
            gap: Gap { prev_token: "cheese".to_owned() },
        }];
        store.handle_linked_chunk_updates(linked_chunk_id, updates).await.unwrap();

        let updates = vec![
            Update::NewGapChunk {
                previous: Some(ChunkIdentifier::new(1)),
                new: ChunkIdentifier::new(3),
                next: None,
                gap: Gap {
                    prev_token: "t9-4880969790_757284974_23234261_m3457690681~38.3457690701_3823363516_264464613_1459149788_11091595867_0_439828".to_owned()
                },
            },
            Update::RemoveChunk(ChunkIdentifier::new(3)),
        ];
        store.handle_linked_chunk_updates(linked_chunk_id, updates).await.unwrap();

        let chunks = store.load_all_chunks(linked_chunk_id).await.unwrap();
        assert_eq!(chunks.len(), 1);
    }

    async fn test_linked_chunk_new_gap_chunk(store: IndexeddbEventCacheStore) {
        let linked_chunk_id = LinkedChunkId::Room(&DEFAULT_TEST_ROOM_ID);
        let updates = vec![Update::NewGapChunk {
            previous: None,
            new: ChunkIdentifier::new(42),
            next: None,
            gap: Gap { prev_token: "raclette".to_owned() },
        }];
        store.handle_linked_chunk_updates(linked_chunk_id, updates).await.unwrap();

        let mut chunks = store.load_all_chunks(linked_chunk_id).await.unwrap();
        assert_eq!(chunks.len(), 1);

        // Chunks are ordered from smaller to bigger IDs.
        let c = chunks.remove(0);
        assert_eq!(c.identifier, ChunkIdentifier::new(42));
        assert_eq!(c.previous, None);
        assert_eq!(c.next, None);
        assert_matches!(c.content, ChunkContent::Gap(gap) => {
            assert_eq!(gap.prev_token, "raclette");
        });
    }

    async fn test_linked_chunk_replace_item(store: IndexeddbEventCacheStore) {
        let linked_chunk_id = LinkedChunkId::Room(&DEFAULT_TEST_ROOM_ID);
        let updates = vec![
            Update::NewItemsChunk { previous: None, new: ChunkIdentifier::new(42), next: None },
            Update::PushItems {
                at: Position::new(ChunkIdentifier::new(42), 0),
                items: vec![
                    make_test_event(linked_chunk_id.room_id(), "hello"),
                    make_test_event(linked_chunk_id.room_id(), "world"),
                ],
            },
            Update::ReplaceItem {
                at: Position::new(ChunkIdentifier::new(42), 1),
                item: make_test_event(linked_chunk_id.room_id(), "yolo"),
            },
        ];
        store.handle_linked_chunk_updates(linked_chunk_id, updates).await.unwrap();

        let mut chunks = store.load_all_chunks(linked_chunk_id).await.unwrap();
        assert_eq!(chunks.len(), 1);

        let c = chunks.remove(0);
        assert_eq!(c.identifier, ChunkIdentifier::new(42));
        assert_eq!(c.previous, None);
        assert_eq!(c.next, None);
        assert_matches!(c.content, ChunkContent::Items(events) => {
            assert_eq!(events.len(), 2);
            check_test_event(&events[0], "hello");
            check_test_event(&events[1], "yolo");
        });
    }

    async fn test_linked_chunk_remove_chunk(store: IndexeddbEventCacheStore) {
        let linked_chunk_id = LinkedChunkId::Room(&DEFAULT_TEST_ROOM_ID);
        let updates = vec![
            Update::NewGapChunk {
                previous: None,
                new: ChunkIdentifier::new(42),
                next: None,
                gap: Gap { prev_token: "raclette".to_owned() },
            },
            Update::NewGapChunk {
                previous: Some(ChunkIdentifier::new(42)),
                new: ChunkIdentifier::new(43),
                next: None,
                gap: Gap { prev_token: "fondue".to_owned() },
            },
            Update::NewGapChunk {
                previous: Some(ChunkIdentifier::new(43)),
                new: ChunkIdentifier::new(44),
                next: None,
                gap: Gap { prev_token: "tartiflette".to_owned() },
            },
            Update::RemoveChunk(ChunkIdentifier::new(43)),
        ];
        store.handle_linked_chunk_updates(linked_chunk_id, updates).await.unwrap();

        let mut chunks = store.load_all_chunks(linked_chunk_id).await.unwrap();
        assert_eq!(chunks.len(), 2);

        // Chunks are ordered from smaller to bigger IDs.
        let c = chunks.remove(0);
        assert_eq!(c.identifier, ChunkIdentifier::new(42));
        assert_eq!(c.previous, None);
        assert_eq!(c.next, Some(ChunkIdentifier::new(44)));
        assert_matches!(c.content, ChunkContent::Gap(gap) => {
            assert_eq!(gap.prev_token, "raclette");
        });

        let c = chunks.remove(0);
        assert_eq!(c.identifier, ChunkIdentifier::new(44));
        assert_eq!(c.previous, Some(ChunkIdentifier::new(42)));
        assert_eq!(c.next, None);
        assert_matches!(c.content, ChunkContent::Gap(gap) => {
            assert_eq!(gap.prev_token, "tartiflette");
        });
    }

    async fn test_linked_chunk_push_items(store: IndexeddbEventCacheStore) {
        let linked_chunk_id = LinkedChunkId::Room(&DEFAULT_TEST_ROOM_ID);
        let updates = vec![
            Update::NewItemsChunk { previous: None, new: ChunkIdentifier::new(42), next: None },
            Update::PushItems {
                at: Position::new(ChunkIdentifier::new(42), 0),
                items: vec![
                    make_test_event(linked_chunk_id.room_id(), "hello"),
                    make_test_event(linked_chunk_id.room_id(), "world"),
                ],
            },
            Update::PushItems {
                at: Position::new(ChunkIdentifier::new(42), 2),
                items: vec![make_test_event(linked_chunk_id.room_id(), "who?")],
            },
        ];
        store.handle_linked_chunk_updates(linked_chunk_id, updates).await.unwrap();

        let mut chunks = store.load_all_chunks(linked_chunk_id).await.unwrap();
        assert_eq!(chunks.len(), 1);

        let c = chunks.remove(0);
        assert_eq!(c.identifier, ChunkIdentifier::new(42));
        assert_eq!(c.previous, None);
        assert_eq!(c.next, None);
        assert_matches!(c.content, ChunkContent::Items(events) => {
            assert_eq!(events.len(), 3);
            check_test_event(&events[0], "hello");
            check_test_event(&events[1], "world");
            check_test_event(&events[2], "who?");
        });
    }

    async fn test_linked_chunk_remove_item(store: IndexeddbEventCacheStore) {
        let linked_chunk_id = LinkedChunkId::Room(&DEFAULT_TEST_ROOM_ID);
        let updates = vec![
            Update::NewItemsChunk { previous: None, new: ChunkIdentifier::new(42), next: None },
            Update::PushItems {
                at: Position::new(ChunkIdentifier::new(42), 0),
                items: vec![
                    make_test_event(linked_chunk_id.room_id(), "hello"),
                    make_test_event(linked_chunk_id.room_id(), "world"),
                ],
            },
            Update::RemoveItem { at: Position::new(ChunkIdentifier::new(42), 0) },
        ];
        store.handle_linked_chunk_updates(linked_chunk_id, updates).await.unwrap();

        let mut chunks = store.load_all_chunks(linked_chunk_id).await.unwrap();
        assert_eq!(chunks.len(), 1);

        let c = chunks.remove(0);
        assert_eq!(c.identifier, ChunkIdentifier::new(42));
        assert_eq!(c.previous, None);
        assert_eq!(c.next, None);
        assert_matches!(c.content, ChunkContent::Items(events) => {
            assert_eq!(events.len(), 1);
            check_test_event(&events[0], "world");
        });
    }

    async fn test_linked_chunk_detach_last_items(store: IndexeddbEventCacheStore) {
        let linked_chunk_id = LinkedChunkId::Room(&DEFAULT_TEST_ROOM_ID);
        let updates = vec![
            Update::NewItemsChunk { previous: None, new: ChunkIdentifier::new(42), next: None },
            Update::PushItems {
                at: Position::new(ChunkIdentifier::new(42), 0),
                items: vec![
                    make_test_event(linked_chunk_id.room_id(), "hello"),
                    make_test_event(linked_chunk_id.room_id(), "world"),
                    make_test_event(linked_chunk_id.room_id(), "howdy"),
                ],
            },
            Update::DetachLastItems { at: Position::new(ChunkIdentifier::new(42), 1) },
        ];
        store.handle_linked_chunk_updates(linked_chunk_id, updates).await.unwrap();

        let mut chunks = store.load_all_chunks(linked_chunk_id).await.unwrap();
        assert_eq!(chunks.len(), 1);

        let c = chunks.remove(0);
        assert_eq!(c.identifier, ChunkIdentifier::new(42));
        assert_eq!(c.previous, None);
        assert_eq!(c.next, None);
        assert_matches!(c.content, ChunkContent::Items(events) => {
            assert_eq!(events.len(), 1);
            check_test_event(&events[0], "hello");
        });
    }

    async fn test_linked_chunk_start_end_reattach_items(store: IndexeddbEventCacheStore) {
        let linked_chunk_id = LinkedChunkId::Room(&DEFAULT_TEST_ROOM_ID);
        // Same updates and checks as test_linked_chunk_push_items, but with extra
        // `StartReattachItems` and `EndReattachItems` updates, which must have no
        // effects.
        let updates = vec![
            Update::NewItemsChunk { previous: None, new: ChunkIdentifier::new(42), next: None },
            Update::PushItems {
                at: Position::new(ChunkIdentifier::new(42), 0),
                items: vec![
                    make_test_event(linked_chunk_id.room_id(), "hello"),
                    make_test_event(linked_chunk_id.room_id(), "world"),
                    make_test_event(linked_chunk_id.room_id(), "howdy"),
                ],
            },
            Update::StartReattachItems,
            Update::EndReattachItems,
        ];
        store.handle_linked_chunk_updates(linked_chunk_id, updates).await.unwrap();

        let mut chunks = store.load_all_chunks(linked_chunk_id).await.unwrap();
        assert_eq!(chunks.len(), 1);

        let c = chunks.remove(0);
        assert_eq!(c.identifier, ChunkIdentifier::new(42));
        assert_eq!(c.previous, None);
        assert_eq!(c.next, None);
        assert_matches!(c.content, ChunkContent::Items(events) => {
            assert_eq!(events.len(), 3);
            check_test_event(&events[0], "hello");
            check_test_event(&events[1], "world");
            check_test_event(&events[2], "howdy");
        });
    }

    async fn test_linked_chunk_clear(store: IndexeddbEventCacheStore) {
        let linked_chunk_id = LinkedChunkId::Room(&DEFAULT_TEST_ROOM_ID);
        let updates = vec![
            Update::NewItemsChunk { previous: None, new: ChunkIdentifier::new(42), next: None },
            Update::NewGapChunk {
                previous: Some(ChunkIdentifier::new(42)),
                new: ChunkIdentifier::new(54),
                next: None,
                gap: Gap { prev_token: "fondue".to_owned() },
            },
            Update::PushItems {
                at: Position::new(ChunkIdentifier::new(42), 0),
                items: vec![
                    make_test_event(linked_chunk_id.room_id(), "hello"),
                    make_test_event(linked_chunk_id.room_id(), "world"),
                    make_test_event(linked_chunk_id.room_id(), "howdy"),
                ],
            },
            Update::Clear,
        ];
        store.handle_linked_chunk_updates(linked_chunk_id, updates).await.unwrap();

        let chunks = store.load_all_chunks(linked_chunk_id).await.unwrap();
        assert!(chunks.is_empty());
    }

    async fn test_linked_chunk_multiple_rooms(store: IndexeddbEventCacheStore) {
        // Check that applying updates to one room doesn't affect the others.
        // Use the same chunk identifier in both rooms to battle-test search.
        let linked_chunk_id1 = LinkedChunkId::Room(room_id!("!realcheeselovers:raclette.fr"));
        let updates1 = vec![
            Update::NewItemsChunk { previous: None, new: ChunkIdentifier::new(42), next: None },
            Update::PushItems {
                at: Position::new(ChunkIdentifier::new(42), 0),
                items: vec![
                    make_test_event(linked_chunk_id1.room_id(), "best cheese is raclette"),
                    make_test_event(linked_chunk_id1.room_id(), "obviously"),
                ],
            },
        ];
        store.handle_linked_chunk_updates(linked_chunk_id1, updates1).await.unwrap();

        let linked_chunk_id2 = LinkedChunkId::Room(room_id!("!realcheeselovers:fondue.ch"));
        let updates2 = vec![
            Update::NewItemsChunk { previous: None, new: ChunkIdentifier::new(42), next: None },
            Update::PushItems {
                at: Position::new(ChunkIdentifier::new(42), 0),
                items: vec![make_test_event(linked_chunk_id1.room_id(), "beaufort is the best")],
            },
        ];
        store.handle_linked_chunk_updates(linked_chunk_id2, updates2).await.unwrap();

        // Check chunks from room 1.
        let mut chunks1 = store.load_all_chunks(linked_chunk_id1).await.unwrap();
        assert_eq!(chunks1.len(), 1);

        let c = chunks1.remove(0);
        assert_matches!(c.content, ChunkContent::Items(events) => {
            assert_eq!(events.len(), 2);
            check_test_event(&events[0], "best cheese is raclette");
            check_test_event(&events[1], "obviously");
        });

        // Check chunks from room 2.
        let mut chunks2 = store.load_all_chunks(linked_chunk_id2).await.unwrap();
        assert_eq!(chunks2.len(), 1);

        let c = chunks2.remove(0);
        assert_matches!(c.content, ChunkContent::Items(events) => {
            assert_eq!(events.len(), 1);
            check_test_event(&events[0], "beaufort is the best");
        });
    }

    async fn test_linked_chunk_update_is_a_transaction(store: IndexeddbEventCacheStore) {
        let linked_chunk_id = LinkedChunkId::Room(*DEFAULT_TEST_ROOM_ID);
        // Trigger a violation of the unique constraint on the (room id, chunk id)
        // couple.
        let updates = vec![
            Update::NewItemsChunk { previous: None, new: ChunkIdentifier::new(42), next: None },
            Update::NewItemsChunk { previous: None, new: ChunkIdentifier::new(42), next: None },
        ];
        let err = store.handle_linked_chunk_updates(linked_chunk_id, updates).await.unwrap_err();

        // The operation fails with a constraint violation error.
        assert_matches!(err, IndexeddbEventCacheStoreError::DomException { .. });

        // If the updates have been handled transactionally, then no new chunks should
        // have been added; failure of the second update leads to the first one being
        // rolled back.
        let chunks = store.load_all_chunks(linked_chunk_id).await.unwrap();
        assert!(chunks.is_empty());
    }

    async fn test_filter_duplicate_events_no_events(store: IndexeddbEventCacheStore) {
        let linked_chunk_id = LinkedChunkId::Room(*DEFAULT_TEST_ROOM_ID);
        let duplicates = store.filter_duplicated_events(linked_chunk_id, Vec::new()).await.unwrap();
        assert!(duplicates.is_empty());
    }

    async fn test_load_last_chunk(store: IndexeddbEventCacheStore) {
        let linked_chunk_id = LinkedChunkId::Room(room_id!("!r0:matrix.org"));
        let event = |msg: &str| make_test_event(linked_chunk_id.room_id(), msg);

        // Case #1: no last chunk.
        let (last_chunk, chunk_identifier_generator) =
            store.load_last_chunk(linked_chunk_id).await.unwrap();
        assert!(last_chunk.is_none());
        assert_eq!(chunk_identifier_generator.current(), 0);

        // Case #2: only one chunk is present.
        let updates = vec![
            Update::NewItemsChunk { previous: None, new: ChunkIdentifier::new(42), next: None },
            Update::PushItems {
                at: Position::new(ChunkIdentifier::new(42), 0),
                items: vec![event("saucisse de morteau"), event("comté")],
            },
        ];
        store.handle_linked_chunk_updates(linked_chunk_id, updates).await.unwrap();

        let (last_chunk, chunk_identifier_generator) =
            store.load_last_chunk(linked_chunk_id).await.unwrap();
        assert_matches!(last_chunk, Some(last_chunk) => {
            assert_eq!(last_chunk.identifier, 42);
            assert!(last_chunk.previous.is_none());
            assert!(last_chunk.next.is_none());
            assert_matches!(last_chunk.content, ChunkContent::Items(items) => {
                assert_eq!(items.len(), 2);
                check_test_event(&items[0], "saucisse de morteau");
                check_test_event(&items[1], "comté");
            });
        });
        assert_eq!(chunk_identifier_generator.current(), 42);

        // Case #3: more chunks are present.
        let updates = vec![
            Update::NewItemsChunk {
                previous: Some(ChunkIdentifier::new(42)),
                new: ChunkIdentifier::new(7),
                next: None,
            },
            Update::PushItems {
                at: Position::new(ChunkIdentifier::new(7), 0),
                items: vec![event("fondue"), event("gruyère"), event("mont d'or")],
            },
        ];
        store.handle_linked_chunk_updates(linked_chunk_id, updates).await.unwrap();

        let (last_chunk, chunk_identifier_generator) =
            store.load_last_chunk(linked_chunk_id).await.unwrap();
        assert_matches!(last_chunk, Some(last_chunk) => {
            assert_eq!(last_chunk.identifier, 7);
            assert_matches!(last_chunk.previous, Some(previous) => {
                assert_eq!(previous, 42);
            });
            assert!(last_chunk.next.is_none());
            assert_matches!(last_chunk.content, ChunkContent::Items(items) => {
                assert_eq!(items.len(), 3);
                check_test_event(&items[0], "fondue");
                check_test_event(&items[1], "gruyère");
                check_test_event(&items[2], "mont d'or");
            });
        });
        assert_eq!(chunk_identifier_generator.current(), 42);
    }

    async fn test_load_last_chunk_with_a_cycle(store: IndexeddbEventCacheStore) {
        let linked_chunk_id = LinkedChunkId::Room(room_id!("!r0:matrix.org"));
        let updates = vec![
            Update::NewItemsChunk { previous: None, new: ChunkIdentifier::new(0), next: None },
            Update::NewItemsChunk {
                // Because `previous` connects to chunk #0, it will create a cycle.
                // Chunk #0 will have a `next` set to chunk #1! Consequently, the last chunk
                // **does not exist**. We have to detect this cycle.
                previous: Some(ChunkIdentifier::new(0)),
                new: ChunkIdentifier::new(1),
                next: Some(ChunkIdentifier::new(0)),
            },
        ];
        store.handle_linked_chunk_updates(linked_chunk_id, updates).await.unwrap();
        store.load_last_chunk(linked_chunk_id).await.unwrap_err();
    }

    async fn test_load_previous_chunk(store: IndexeddbEventCacheStore) {
        let linked_chunk_id = LinkedChunkId::Room(room_id!("!r0:matrix.org"));
        let event = |msg: &str| make_test_event(linked_chunk_id.room_id(), msg);

        // Case #1: no chunk at all, equivalent to having an nonexistent
        // `before_chunk_identifier`.
        let previous_chunk =
            store.load_previous_chunk(linked_chunk_id, ChunkIdentifier::new(153)).await.unwrap();
        assert!(previous_chunk.is_none());

        // Case #2: there is one chunk only: we request the previous on this
        // one, it doesn't exist.
        let updates = vec![Update::NewItemsChunk {
            previous: None,
            new: ChunkIdentifier::new(42),
            next: None,
        }];
        store.handle_linked_chunk_updates(linked_chunk_id, updates).await.unwrap();

        let previous_chunk =
            store.load_previous_chunk(linked_chunk_id, ChunkIdentifier::new(42)).await.unwrap();
        assert!(previous_chunk.is_none());

        // Case #3: there are two chunks.
        let updates = vec![
            // new chunk before the one that exists.
            Update::NewItemsChunk {
                previous: None,
                new: ChunkIdentifier::new(7),
                next: Some(ChunkIdentifier::new(42)),
            },
            Update::PushItems {
                at: Position::new(ChunkIdentifier::new(7), 0),
                items: vec![event("brigand du jorat"), event("morbier")],
            },
        ];
        store.handle_linked_chunk_updates(linked_chunk_id, updates).await.unwrap();

        let previous_chunk =
            store.load_previous_chunk(linked_chunk_id, ChunkIdentifier::new(42)).await.unwrap();

        assert_matches!(previous_chunk, Some(previous_chunk) => {
            assert_eq!(previous_chunk.identifier, 7);
            assert!(previous_chunk.previous.is_none());
            assert_matches!(previous_chunk.next, Some(next) => {
                assert_eq!(next, 42);
            });
            assert_matches!(previous_chunk.content, ChunkContent::Items(items) => {
                assert_eq!(items.len(), 2);
                check_test_event(&items[0], "brigand du jorat");
                check_test_event(&items[1], "morbier");
            });
        });
    }

    mod unencrypted {
        use matrix_sdk_base::{
            event_cache::store::EventCacheStoreError, event_cache_store_integration_tests,
            event_cache_store_integration_tests_time,
        };
        use matrix_sdk_test::async_test;
        use uuid::Uuid;

        use crate::IndexeddbEventCacheStore;

        wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

        async fn get_event_cache_store() -> Result<IndexeddbEventCacheStore, EventCacheStoreError> {
            let name = format!("test-event-cache-store-{}", Uuid::new_v4().as_hyphenated());
            Ok(IndexeddbEventCacheStore::builder().name(name).build().await?)
        }

        #[cfg(target_arch = "wasm32")]
        event_cache_store_integration_tests!();

        #[cfg(target_arch = "wasm32")]
        event_cache_store_integration_tests_time!();

        #[async_test]
        async fn test_linked_chunk_new_items_chunk() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_new_items_chunk(store).await
        }

        #[async_test]
        async fn test_add_gap_chunk_and_delete_it_immediately() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_add_gap_chunk_and_delete_it_immediately(store).await
        }

        #[async_test]
        async fn test_linked_chunk_new_gap_chunk() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_new_gap_chunk(store).await
        }

        #[async_test]
        async fn test_linked_chunk_replace_item() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_replace_item(store).await
        }

        #[async_test]
        async fn test_linked_chunk_remove_chunk() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_remove_chunk(store).await
        }

        #[async_test]
        async fn test_linked_chunk_push_items() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_push_items(store).await
        }

        #[async_test]
        async fn test_linked_chunk_remove_item() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_remove_item(store).await
        }

        #[async_test]
        async fn test_linked_chunk_detach_last_items() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_detach_last_items(store).await
        }

        #[async_test]
        async fn test_linked_chunk_start_end_reattach_items() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_start_end_reattach_items(store).await
        }

        #[async_test]
        async fn test_linked_chunk_clear() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_clear(store).await
        }

        #[async_test]
        async fn test_linked_chunk_multiple_rooms() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_multiple_rooms(store).await
        }

        #[async_test]
        async fn test_linked_chunk_update_is_a_transaction() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_update_is_a_transaction(store).await
        }

        #[async_test]
        async fn test_filter_duplicate_events_no_events() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_filter_duplicate_events_no_events(store).await
        }

        #[async_test]
        async fn test_load_last_chunk() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_load_last_chunk(store).await
        }

        #[async_test]
        async fn test_load_last_chunk_with_a_cycle() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_load_last_chunk_with_a_cycle(store).await
        }

        #[async_test]
        async fn test_load_previous_chunk() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_load_previous_chunk(store).await
        }
    }

    mod encrypted {
        use std::sync::Arc;

        use matrix_sdk_base::{
            event_cache::store::EventCacheStoreError, event_cache_store_integration_tests,
            event_cache_store_integration_tests_time,
        };
        use matrix_sdk_store_encryption::StoreCipher;
        use matrix_sdk_test::async_test;
        use uuid::Uuid;

        use crate::IndexeddbEventCacheStore;

        wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

        async fn get_event_cache_store() -> Result<IndexeddbEventCacheStore, EventCacheStoreError> {
            let name = format!("test-event-cache-store-{}", Uuid::new_v4().as_hyphenated());
            Ok(IndexeddbEventCacheStore::builder()
                .name(name)
                .store_cipher(Arc::new(StoreCipher::new()?))
                .build()
                .await?)
        }

        #[cfg(target_arch = "wasm32")]
        event_cache_store_integration_tests!();

        #[cfg(target_arch = "wasm32")]
        event_cache_store_integration_tests_time!();

        #[async_test]
        async fn test_linked_chunk_new_items_chunk() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_new_items_chunk(store).await
        }

        #[async_test]
        async fn test_add_gap_chunk_and_delete_it_immediately() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_add_gap_chunk_and_delete_it_immediately(store).await
        }

        #[async_test]
        async fn test_linked_chunk_new_gap_chunk() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_new_gap_chunk(store).await
        }

        #[async_test]
        async fn test_linked_chunk_replace_item() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_replace_item(store).await
        }

        #[async_test]
        async fn test_linked_chunk_remove_chunk() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_remove_chunk(store).await
        }

        #[async_test]
        async fn test_linked_chunk_push_items() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_push_items(store).await
        }

        #[async_test]
        async fn test_linked_chunk_remove_item() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_remove_item(store).await
        }

        #[async_test]
        async fn test_linked_chunk_detach_last_items() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_detach_last_items(store).await
        }

        #[async_test]
        async fn test_linked_chunk_start_end_reattach_items() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_start_end_reattach_items(store).await
        }

        #[async_test]
        async fn test_linked_chunk_clear() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_clear(store).await
        }

        #[async_test]
        async fn test_linked_chunk_multiple_rooms() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_multiple_rooms(store).await
        }

        #[async_test]
        async fn test_linked_chunk_update_is_a_transaction() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_linked_chunk_update_is_a_transaction(store).await
        }

        #[async_test]
        async fn test_filter_duplicate_events_no_events() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_filter_duplicate_events_no_events(store).await
        }

        #[async_test]
        async fn test_load_last_chunk() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_load_last_chunk(store).await
        }

        #[async_test]
        async fn test_load_last_chunk_with_a_cycle() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_load_last_chunk_with_a_cycle(store).await
        }

        #[async_test]
        async fn test_load_previous_chunk() {
            let store = get_event_cache_store().await.expect("Failed to get event cache store");
            super::test_load_previous_chunk(store).await
        }
    }
}
