use indexed_db_futures::{prelude::IdbTransaction, IdbQuerySource};
use matrix_sdk_base::{
    event_cache::{Event as RawEvent, Gap as RawGap},
    linked_chunk::{ChunkContent, ChunkIdentifier, RawChunk},
    SendOutsideWasm, SyncOutsideWasm,
};
use ruma::{events::relation::RelationType, OwnedEventId, RoomId};
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use web_sys::IdbCursorDirection;

use crate::event_cache_store::{
    serializer::{
        types::{
            Indexed, IndexedChunkIdKey, IndexedEventIdKey, IndexedEventPositionKey,
            IndexedEventRelationKey, IndexedGapIdKey, IndexedKey, IndexedKeyBounds,
            IndexedNextChunkIdKey,
        },
        IndexeddbEventCacheStoreSerializer,
    },
    types::{Chunk, ChunkType, Event, Gap, Position},
};

pub trait WasmCompatibleAsyncError:
    std::error::Error + SendOutsideWasm + SyncOutsideWasm + 'static
{
}

impl<T> WasmCompatibleAsyncError for T where
    T: std::error::Error + SendOutsideWasm + SyncOutsideWasm + 'static
{
}

#[derive(Debug, Error)]
pub enum EventCacheStoreTransactionError {
    #[error("DomException {name} ({code}): {message}")]
    DomException { name: String, message: String, code: u16 },
    #[error("serialization: {0}")]
    Serialization(Box<dyn WasmCompatibleAsyncError>),
    #[error("item is not unique")]
    ItemIsNotUnique,
    #[error("item not found")]
    ItemNotFound,
}

impl From<web_sys::DomException> for EventCacheStoreTransactionError {
    fn from(value: web_sys::DomException) -> Self {
        Self::DomException { name: value.name(), message: value.message(), code: value.code() }
    }
}

impl From<serde_wasm_bindgen::Error> for EventCacheStoreTransactionError {
    fn from(e: serde_wasm_bindgen::Error) -> Self {
        Self::Serialization(Box::new(<serde_json::Error as serde::de::Error>::custom(
            e.to_string(),
        )))
    }
}

#[derive(Debug)]
pub struct EventCacheStoreTransaction<'a> {
    transaction: IdbTransaction<'a>,
    serializer: &'a IndexeddbEventCacheStoreSerializer,
}

impl<'a> EventCacheStoreTransaction<'a> {
    pub fn new(
        transaction: IdbTransaction<'a>,
        serializer: &'a IndexeddbEventCacheStoreSerializer,
    ) -> Self {
        Self { transaction, serializer }
    }

    pub fn into_inner(self) -> IdbTransaction<'a> {
        self.transaction
    }

    pub async fn execute(self) -> Result<(), EventCacheStoreTransactionError> {
        self.transaction.await.into_result().map_err(Into::into)
    }

    pub async fn get_items_by_key<T, K>(
        &self,
        room_id: &RoomId,
        lower: &K::KeyComponents,
        upper: &K::KeyComponents,
    ) -> Result<Vec<T>, EventCacheStoreTransactionError>
    where
        T: Indexed,
        T::IndexedType: DeserializeOwned,
        T::Error: WasmCompatibleAsyncError,
        K: IndexedKey<T> + IndexedKeyBounds<T> + Serialize,
    {
        let range = self.serializer.encode_key_range_from_to::<T, K>(room_id, lower, upper)?;
        let object_store = self.transaction.object_store(T::OBJECT_STORE)?;
        let array = if let Some(index) = K::index() {
            object_store.index(index)?.get_all_with_key(&range)?.await?
        } else {
            object_store.get_all_with_key(&range)?.await?
        };
        let mut items = Vec::new();
        for value in array {
            let item = self
                .serializer
                .deserialize(value)
                .map_err(|e| EventCacheStoreTransactionError::Serialization(Box::new(e)))?;
            items.push(item);
        }
        Ok(items)
    }

    pub async fn get_items_in_room<T, K>(
        &self,
        room_id: &RoomId,
    ) -> Result<Vec<T>, EventCacheStoreTransactionError>
    where
        T: Indexed,
        T::IndexedType: DeserializeOwned,
        T::Error: WasmCompatibleAsyncError,
        K: IndexedKeyBounds<T> + Serialize,
    {
        let range = self.serializer.encode_key_range::<T, K>(room_id)?;
        let object_store = self.transaction.object_store(T::OBJECT_STORE)?;
        let array = if let Some(index) = K::index() {
            object_store.index(index)?.get_all_with_key(&range)?.await?
        } else {
            object_store.get_all_with_key(&range)?.await?
        };
        let mut items = Vec::new();
        for value in array {
            let item = self
                .serializer
                .deserialize(value)
                .map_err(|e| EventCacheStoreTransactionError::Serialization(Box::new(e)))?;
            items.push(item);
        }
        Ok(items)
    }

    pub async fn get_item_by_key<T, K>(
        &self,
        room_id: &RoomId,
        components: &K::KeyComponents,
    ) -> Result<Option<T>, EventCacheStoreTransactionError>
    where
        T: Indexed,
        T::IndexedType: DeserializeOwned,
        T::Error: WasmCompatibleAsyncError,
        K: IndexedKey<T> + IndexedKeyBounds<T> + Serialize,
    {
        let mut items = self.get_items_by_key::<T, K>(room_id, components, components).await?;
        if items.len() > 1 {
            return Err(EventCacheStoreTransactionError::ItemIsNotUnique);
        }
        Ok(items.pop())
    }

    pub async fn get_items_count_by_key<T, K>(
        &self,
        room_id: &RoomId,
        lower: &K::KeyComponents,
        upper: &K::KeyComponents,
    ) -> Result<usize, EventCacheStoreTransactionError>
    where
        T: Indexed,
        T::IndexedType: DeserializeOwned,
        T::Error: WasmCompatibleAsyncError,
        K: IndexedKeyBounds<T> + Serialize,
    {
        let range = self.serializer.encode_key_range_from_to::<T, K>(room_id, lower, upper)?;
        let object_store = self.transaction.object_store(T::OBJECT_STORE)?;
        let count = if let Some(index) = K::index() {
            object_store.index(index)?.count_with_key(&range)?.await?
        } else {
            object_store.count_with_key(&range)?.await?
        };
        Ok(count as usize)
    }

    pub async fn get_items_count_in_room<T, K>(
        &self,
        room_id: &RoomId,
    ) -> Result<usize, EventCacheStoreTransactionError>
    where
        T: Indexed,
        T::IndexedType: DeserializeOwned,
        T::Error: WasmCompatibleAsyncError,
        K: IndexedKeyBounds<T> + Serialize,
    {
        self.get_items_count_by_key::<T, K>(
            room_id,
            &K::lower_key_components(),
            &K::upper_key_components(),
        )
        .await
    }

    pub async fn get_max_item_by_key<T, K>(
        &self,
        room_id: &RoomId,
    ) -> Result<Option<T>, EventCacheStoreTransactionError>
    where
        T: Indexed,
        T::IndexedType: DeserializeOwned,
        T::Error: WasmCompatibleAsyncError,
        K: IndexedKey<T> + IndexedKeyBounds<T> + Serialize,
    {
        let range = self.serializer.encode_key_range::<T, K>(room_id)?;
        let direction = IdbCursorDirection::Prev;
        let object_store = self.transaction.object_store(T::OBJECT_STORE)?;
        if let Some(index) = K::index() {
            object_store
                .index(index)?
                .open_cursor_with_range_and_direction(&range, direction)?
                .await?
                .map(|cursor| self.serializer.deserialize(cursor.value()))
                .transpose()
                .map_err(|e| EventCacheStoreTransactionError::Serialization(Box::new(e)))
        } else {
            object_store
                .open_cursor_with_range_and_direction(&range, direction)?
                .await?
                .map(|cursor| self.serializer.deserialize(cursor.value()))
                .transpose()
                .map_err(|e| EventCacheStoreTransactionError::Serialization(Box::new(e)))
        }
    }

    pub async fn add_item<T>(
        &self,
        room_id: &RoomId,
        item: &T,
    ) -> Result<(), EventCacheStoreTransactionError>
    where
        T: Indexed + Serialize,
        T::IndexedType: Serialize,
        T::Error: WasmCompatibleAsyncError,
    {
        self.transaction
            .object_store(T::OBJECT_STORE)?
            .add_val_owned(
                self.serializer
                    .serialize(room_id, item)
                    .map_err(|e| EventCacheStoreTransactionError::Serialization(Box::new(e)))?,
            )?
            .await
            .map_err(Into::into)
    }

    pub async fn put_item<T>(
        &self,
        room_id: &RoomId,
        item: &T,
    ) -> Result<(), EventCacheStoreTransactionError>
    where
        T: Indexed + Serialize,
        T::IndexedType: Serialize,
        T::Error: WasmCompatibleAsyncError,
    {
        self.transaction
            .object_store(T::OBJECT_STORE)?
            .put_val_owned(
                self.serializer
                    .serialize(room_id, item)
                    .map_err(|e| EventCacheStoreTransactionError::Serialization(Box::new(e)))?,
            )?
            .await
            .map_err(Into::into)
    }

    pub async fn delete_items_by_key<T, K>(
        &self,
        room_id: &RoomId,
        lower: &K::KeyComponents,
        upper: &K::KeyComponents,
    ) -> Result<(), EventCacheStoreTransactionError>
    where
        T: Indexed,
        K: IndexedKeyBounds<T> + Serialize,
    {
        let range = self.serializer.encode_key_range_from_to::<T, K>(room_id, lower, upper)?;
        let object_store = self.transaction.object_store(T::OBJECT_STORE)?;
        if let Some(index) = K::index() {
            let index = object_store.index(index)?;
            if let Some(cursor) = index.open_cursor_with_range(&range)?.await? {
                while cursor.key().is_some() {
                    cursor.delete()?.await?;
                    cursor.continue_cursor()?.await?;
                }
            }
        } else {
            object_store.delete_owned(&range)?.await?;
        }
        Ok(())
    }

    pub async fn delete_items_in_room<T, K>(
        &self,
        room_id: &RoomId,
    ) -> Result<(), EventCacheStoreTransactionError>
    where
        T: Indexed,
        K: IndexedKeyBounds<T> + Serialize,
    {
        let range = self.serializer.encode_key_range::<T, K>(room_id)?;
        let object_store = self.transaction.object_store(T::OBJECT_STORE)?;
        if let Some(index) = K::index() {
            let index = object_store.index(index)?;
            if let Some(cursor) = index.open_cursor_with_range(&range)?.await? {
                while cursor.key().is_some() {
                    cursor.delete()?.await?;
                    cursor.continue_cursor()?.await?;
                }
            }
        } else {
            object_store.delete_owned(&range)?.await?;
        }
        Ok(())
    }

    pub async fn delete_item_by_key<T, K>(
        &self,
        room_id: &RoomId,
        key: &K::KeyComponents,
    ) -> Result<(), EventCacheStoreTransactionError>
    where
        T: Indexed,
        K: IndexedKeyBounds<T> + Serialize,
    {
        self.delete_items_by_key::<T, K>(room_id, key, key).await
    }

    pub async fn get_chunks_by_id(
        &self,
        room_id: &RoomId,
        lower: &ChunkIdentifier,
        upper: &ChunkIdentifier,
    ) -> Result<Vec<Chunk>, EventCacheStoreTransactionError> {
        self.get_items_by_key::<Chunk, IndexedChunkIdKey>(room_id, lower, upper).await
    }

    pub async fn delete_chunks_by_id(
        &self,
        room_id: &RoomId,
        lower: &ChunkIdentifier,
        upper: &ChunkIdentifier,
    ) -> Result<(), EventCacheStoreTransactionError> {
        self.delete_items_by_key::<Chunk, IndexedChunkIdKey>(room_id, lower, upper).await
    }

    pub async fn get_chunks_in_room(
        &self,
        room_id: &RoomId,
    ) -> Result<Vec<Chunk>, EventCacheStoreTransactionError> {
        self.get_items_in_room::<Chunk, IndexedChunkIdKey>(room_id).await
    }

    pub async fn get_chunks_count_in_room(
        &self,
        room_id: &RoomId,
    ) -> Result<usize, EventCacheStoreTransactionError> {
        self.get_items_count_in_room::<Chunk, IndexedChunkIdKey>(room_id).await
    }

    pub async fn delete_chunks_in_room(
        &self,
        room_id: &RoomId,
    ) -> Result<(), EventCacheStoreTransactionError> {
        self.delete_items_in_room::<Chunk, IndexedChunkIdKey>(room_id).await
    }

    pub async fn get_chunk_by_id(
        &self,
        room_id: &RoomId,
        chunk_id: &ChunkIdentifier,
    ) -> Result<Option<Chunk>, EventCacheStoreTransactionError> {
        self.get_item_by_key::<Chunk, IndexedChunkIdKey>(room_id, chunk_id).await
    }

    pub async fn get_max_chunk_by_id(
        &self,
        room_id: &RoomId,
    ) -> Result<Option<Chunk>, EventCacheStoreTransactionError> {
        self.get_max_item_by_key::<Chunk, IndexedChunkIdKey>(room_id).await
    }

    pub async fn load_chunk_by_id(
        &self,
        room_id: &RoomId,
        chunk_id: &ChunkIdentifier,
    ) -> Result<Option<RawChunk<RawEvent, RawGap>>, EventCacheStoreTransactionError> {
        if let Some(chunk) = self.get_chunk_by_id(room_id, chunk_id).await? {
            let content = match chunk.chunk_type {
                ChunkType::Event => {
                    let events = self
                        .get_events_by_chunk(room_id, &ChunkIdentifier::new(chunk.identifier))
                        .await?
                        .into_iter()
                        .map(|event| event.take_content())
                        .collect();
                    ChunkContent::Items(events)
                }
                ChunkType::Gap => {
                    let gap = self
                        .get_gap_by_id(room_id, &ChunkIdentifier::new(chunk.identifier))
                        .await?
                        .ok_or(EventCacheStoreTransactionError::ItemNotFound)?;
                    ChunkContent::Gap(RawGap { prev_token: gap.prev_token })
                }
            };
            return Ok(Some(RawChunk {
                identifier: ChunkIdentifier::new(chunk.identifier),
                content,
                previous: chunk.previous.map(ChunkIdentifier::new),
                next: chunk.next.map(ChunkIdentifier::new),
            }));
        }
        Ok(None)
    }

    pub async fn add_chunk(
        &self,
        room_id: &RoomId,
        chunk: &Chunk,
    ) -> Result<(), EventCacheStoreTransactionError> {
        self.add_item(room_id, chunk).await?;
        if let Some(previous) = chunk.previous {
            let previous_identifier = ChunkIdentifier::new(previous);
            if let Some(mut previous_chunk) =
                self.get_chunk_by_id(room_id, &previous_identifier).await?
            {
                previous_chunk.next = Some(chunk.identifier);
                self.put_item(room_id, &previous_chunk).await?;
            }
        }
        if let Some(next) = chunk.next {
            let next_identifier = ChunkIdentifier::new(next);
            if let Some(mut next_chunk) = self.get_chunk_by_id(room_id, &next_identifier).await? {
                next_chunk.previous = Some(chunk.identifier);
                self.put_item(room_id, &next_chunk).await?;
            }
        }
        Ok(())
    }

    pub async fn delete_chunk_by_id(
        &self,
        room_id: &RoomId,
        chunk_id: &ChunkIdentifier,
    ) -> Result<(), EventCacheStoreTransactionError> {
        if let Some(chunk) = self.get_chunk_by_id(room_id, chunk_id).await? {
            if let Some(previous) = chunk.previous {
                let previous_identifier = ChunkIdentifier::new(previous);
                if let Some(mut previous_chunk) =
                    self.get_chunk_by_id(room_id, &previous_identifier).await?
                {
                    previous_chunk.next = chunk.next;
                    self.put_item(room_id, &previous_chunk).await?;
                }
            }
            if let Some(next) = chunk.next {
                let next_identifier = ChunkIdentifier::new(next);
                if let Some(mut next_chunk) =
                    self.get_chunk_by_id(room_id, &next_identifier).await?
                {
                    next_chunk.previous = chunk.previous;
                    self.put_item(room_id, &next_chunk).await?;
                }
            }
            self.delete_item_by_key::<Chunk, IndexedChunkIdKey>(room_id, chunk_id).await?;
        }
        Ok(())
    }

    pub async fn get_chunks_by_next_chunk_id(
        &self,
        room_id: &RoomId,
        lower: &Option<ChunkIdentifier>,
        upper: &Option<ChunkIdentifier>,
    ) -> Result<Vec<Chunk>, EventCacheStoreTransactionError> {
        self.get_items_by_key::<Chunk, IndexedNextChunkIdKey>(room_id, lower, upper).await
    }

    pub async fn get_chunk_by_next_chunk_id(
        &self,
        room_id: &RoomId,
        next_chunk_id: &Option<ChunkIdentifier>,
    ) -> Result<Option<Chunk>, EventCacheStoreTransactionError> {
        self.get_item_by_key::<Chunk, IndexedNextChunkIdKey>(room_id, next_chunk_id).await
    }

    pub async fn get_events_by_id(
        &self,
        room_id: &RoomId,
        lower: &OwnedEventId,
        upper: &OwnedEventId,
    ) -> Result<Vec<Event>, EventCacheStoreTransactionError> {
        self.get_items_by_key::<Event, IndexedEventIdKey>(room_id, lower, upper).await
    }

    pub async fn get_event_by_id(
        &self,
        room_id: &RoomId,
        event_id: &OwnedEventId,
    ) -> Result<Option<Event>, EventCacheStoreTransactionError> {
        self.get_item_by_key::<Event, IndexedEventIdKey>(room_id, event_id).await
    }

    pub async fn get_events_by_position(
        &self,
        room_id: &RoomId,
        lower: &Position,
        upper: &Position,
    ) -> Result<Vec<Event>, EventCacheStoreTransactionError> {
        self.get_items_by_key::<Event, IndexedEventPositionKey>(room_id, lower, upper).await
    }

    pub async fn delete_events_by_position(
        &self,
        room_id: &RoomId,
        lower: &Position,
        upper: &Position,
    ) -> Result<(), EventCacheStoreTransactionError> {
        self.delete_items_by_key::<Event, IndexedEventPositionKey>(room_id, lower, upper).await
    }

    pub async fn get_event_by_position(
        &self,
        room_id: &RoomId,
        position: &Position,
    ) -> Result<Option<Event>, EventCacheStoreTransactionError> {
        self.get_item_by_key::<Event, IndexedEventPositionKey>(room_id, position).await
    }

    pub async fn delete_event_by_position(
        &self,
        room_id: &RoomId,
        position: &Position,
    ) -> Result<(), EventCacheStoreTransactionError> {
        self.delete_item_by_key::<Event, IndexedEventPositionKey>(room_id, position).await
    }

    pub async fn delete_event_by_id(
        &self,
        room_id: &RoomId,
        event_id: &OwnedEventId,
    ) -> Result<(), EventCacheStoreTransactionError> {
        self.delete_item_by_key::<Event, IndexedEventIdKey>(room_id, event_id).await
    }

    pub async fn get_events_by_chunk(
        &self,
        room_id: &RoomId,
        chunk_id: &ChunkIdentifier,
    ) -> Result<Vec<Event>, EventCacheStoreTransactionError> {
        self.get_events_by_position(
            room_id,
            &Position { chunk_identifier: chunk_id.index(), index: 0 },
            &Position { chunk_identifier: chunk_id.index(), index: usize::MAX },
        )
        .await
    }

    pub async fn delete_events_by_chunk(
        &self,
        room_id: &RoomId,
        chunk_id: &ChunkIdentifier,
    ) -> Result<(), EventCacheStoreTransactionError> {
        self.delete_events_by_position(
            room_id,
            &Position { chunk_identifier: chunk_id.index(), index: 0 },
            &Position { chunk_identifier: chunk_id.index(), index: usize::MAX },
        )
        .await
    }

    pub async fn get_events_by_chunk_from_index(
        &self,
        room_id: &RoomId,
        position: &Position,
    ) -> Result<Vec<Event>, EventCacheStoreTransactionError> {
        self.get_events_by_position(
            room_id,
            position,
            &Position { chunk_identifier: position.chunk_identifier, index: usize::MAX },
        )
        .await
    }

    pub async fn delete_events_by_chunk_from_index(
        &self,
        room_id: &RoomId,
        position: &Position,
    ) -> Result<(), EventCacheStoreTransactionError> {
        self.delete_events_by_position(
            room_id,
            position,
            &Position { chunk_identifier: position.chunk_identifier, index: usize::MAX },
        )
        .await
    }

    pub async fn get_events_by_relation(
        &self,
        room_id: &RoomId,
        lower: &(OwnedEventId, RelationType),
        upper: &(OwnedEventId, RelationType),
    ) -> Result<Vec<Event>, EventCacheStoreTransactionError> {
        self.get_items_by_key::<Event, IndexedEventRelationKey>(room_id, lower, upper).await
    }

    pub async fn get_event_by_relation(
        &self,
        room_id: &RoomId,
        position: &(OwnedEventId, RelationType),
    ) -> Result<Option<Event>, EventCacheStoreTransactionError> {
        self.get_item_by_key::<Event, IndexedEventRelationKey>(room_id, position).await
    }

    pub async fn get_events_by_related_event(
        &self,
        room_id: &RoomId,
        related_event_id: &OwnedEventId,
    ) -> Result<Vec<Event>, EventCacheStoreTransactionError> {
        let lower = IndexedEventRelationKey::lower_key_components();
        let upper = IndexedEventRelationKey::upper_key_components();
        self.get_items_by_key::<Event, IndexedEventRelationKey>(
            room_id,
            &(related_event_id.clone(), lower.1),
            &(related_event_id.clone(), upper.1),
        )
        .await
    }

    pub async fn delete_events_in_room(
        &self,
        room_id: &RoomId,
    ) -> Result<(), EventCacheStoreTransactionError> {
        self.delete_items_in_room::<Event, IndexedEventIdKey>(room_id).await
    }

    pub async fn put_event(
        &self,
        room_id: &RoomId,
        event: &Event,
    ) -> Result<(), EventCacheStoreTransactionError> {
        if let Some(position) = event.position() {
            // For some reason, we can't simply replace an event with `put_item`
            // because we get an error stating that the data violates a uniqueness
            // constraint on the `events_position` index. So, we delete the event
            // and then call `put_item`.
            //
            // What's even stranger is that if we delete the event through any other
            // key, then `put_item` fails for the same reason. This is unexpected behavior
            // as deleting an object should automatically update all the indexes.
            self.delete_event_by_position(room_id, &position).await?;
        }
        self.put_item(room_id, event).await
    }

    pub async fn get_gaps_by_id(
        &self,
        room_id: &RoomId,
        lower: &ChunkIdentifier,
        upper: &ChunkIdentifier,
    ) -> Result<Vec<Gap>, EventCacheStoreTransactionError> {
        self.get_items_by_key::<Gap, IndexedGapIdKey>(room_id, lower, upper).await
    }

    pub async fn get_gap_by_id(
        &self,
        room_id: &RoomId,
        chunk_id: &ChunkIdentifier,
    ) -> Result<Option<Gap>, EventCacheStoreTransactionError> {
        self.get_item_by_key::<Gap, IndexedGapIdKey>(room_id, chunk_id).await
    }

    pub async fn delete_gaps_in_room(
        &self,
        room_id: &RoomId,
    ) -> Result<(), EventCacheStoreTransactionError> {
        self.delete_items_in_room::<Gap, IndexedGapIdKey>(room_id).await
    }

    pub async fn clear<T>(&self) -> Result<(), EventCacheStoreTransactionError>
    where
        T: Indexed,
    {
        self.transaction.object_store(T::OBJECT_STORE)?.clear()?.await.map_err(Into::into)
    }
}
