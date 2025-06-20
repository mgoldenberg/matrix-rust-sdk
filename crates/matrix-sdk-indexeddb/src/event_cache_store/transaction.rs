use indexed_db_futures::{prelude::IdbTransaction, IdbQuerySource};
use matrix_sdk_base::linked_chunk::ChunkIdentifier;
use ruma::{events::relation::RelationType, OwnedEventId, RoomId};
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use web_sys::IdbCursorDirection;

use crate::{
    event_cache_store::{
        serializer::{
            types::{
                Indexed, IndexedChunkIdKey, IndexedEventIdKey, IndexedEventPositionKey,
                IndexedEventRelationKey, IndexedKey, IndexedKeyBounds, IndexedPartialKeyBounds,
            },
            IndexeddbEventCacheStoreSerializer,
        },
        types::{Chunk, Event, Position},
    },
    IndexeddbEventCacheStoreError,
};

#[derive(Debug, Error)]
pub enum EventCacheStoreTransactionError {
    #[error("DomException {name} ({code}): {message}")]
    DomException { name: String, message: String, code: u16 },
    #[error("serialization: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("found duplicate item")]
    FoundDuplicateItem,
    #[error("indexeddb event cache store: {0}")]
    IndexeddbEventCacheStore(#[from] IndexeddbEventCacheStoreError),
}

impl From<web_sys::DomException> for EventCacheStoreTransactionError {
    fn from(value: web_sys::DomException) -> Self {
        Self::DomException { name: value.name(), message: value.message(), code: value.code() }
    }
}

impl From<serde_wasm_bindgen::Error> for EventCacheStoreTransactionError {
    fn from(e: serde_wasm_bindgen::Error) -> Self {
        Self::Serialization(serde::de::Error::custom(e.to_string()))
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

    pub async fn get_items_by_key<T, K>(
        &self,
        room_id: &RoomId,
        lower: &K::KeyComponents,
        upper: &K::KeyComponents,
    ) -> Result<Vec<T>, EventCacheStoreTransactionError>
    where
        T: Indexed,
        T::IndexedType: DeserializeOwned,
        T::Error: Into<IndexeddbEventCacheStoreError>,
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
            let item = self.serializer.deserialize(value)?;
            items.push(item);
        }
        Ok(items)
    }

    pub async fn get_items_by_partial_key<T, K, C>(
        &self,
        room_id: &RoomId,
        components: &C,
    ) -> Result<Vec<T>, EventCacheStoreTransactionError>
    where
        T: Indexed,
        T::IndexedType: DeserializeOwned,
        T::Error: Into<IndexeddbEventCacheStoreError>,
        K: IndexedPartialKeyBounds<T, C> + Serialize,
    {
        let range = self.serializer.encode_partial_key_range::<T, K, C>(room_id, components)?;
        let object_store = self.transaction.object_store(T::OBJECT_STORE)?;
        let array = if let Some(index) = K::index() {
            object_store.index(index)?.get_all_with_key(&range)?.await?
        } else {
            object_store.get_all_with_key(&range)?.await?
        };
        let mut items = Vec::new();
        for value in array {
            let item = self.serializer.deserialize(value)?;
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
        T::Error: Into<IndexeddbEventCacheStoreError>,
        K: IndexedKey<T> + IndexedKeyBounds<T> + Serialize,
    {
        let mut items = self.get_items_by_key::<T, K>(room_id, components, components).await?;
        if items.len() > 1 {
            return Err(EventCacheStoreTransactionError::FoundDuplicateItem);
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
        T::Error: Into<IndexeddbEventCacheStoreError>,
        K: IndexedKey<T> + IndexedKeyBounds<T> + Serialize,
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

    pub async fn get_max_item_by_key<T, K>(
        &self,
        room_id: &RoomId,
    ) -> Result<Option<T>, EventCacheStoreTransactionError>
    where
        T: Indexed,
        T::IndexedType: DeserializeOwned,
        T::Error: Into<IndexeddbEventCacheStoreError>,
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
                .map_err(Into::into)
        } else {
            object_store
                .open_cursor_with_range_and_direction(&range, direction)?
                .await?
                .map(|cursor| self.serializer.deserialize(cursor.value()))
                .transpose()
                .map_err(Into::into)
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
        T::Error: Into<IndexeddbEventCacheStoreError>,
    {
        self.transaction
            .object_store(T::OBJECT_STORE)?
            .add_val_owned(self.serializer.serialize(room_id, item)?)?
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
        T::Error: Into<IndexeddbEventCacheStoreError>,
    {
        self.transaction
            .object_store(T::OBJECT_STORE)?
            .put_val_owned(self.serializer.serialize(room_id, item)?)?
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

    pub async fn get_chunks_by_id(
        &self,
        room_id: &RoomId,
        lower: &ChunkIdentifier,
        upper: &ChunkIdentifier,
    ) -> Result<Vec<Chunk>, EventCacheStoreTransactionError> {
        self.get_items_by_key::<Chunk, IndexedChunkIdKey>(room_id, lower, upper).await
    }

    pub async fn get_chunk_by_id(
        &self,
        room_id: &RoomId,
        chunk_id: &ChunkIdentifier,
    ) -> Result<Option<Chunk>, EventCacheStoreTransactionError> {
        self.get_item_by_key::<Chunk, IndexedChunkIdKey>(room_id, chunk_id).await
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

    pub async fn get_event_by_position(
        &self,
        room_id: &RoomId,
        position: &Position,
    ) -> Result<Option<Event>, EventCacheStoreTransactionError> {
        self.get_item_by_key::<Event, IndexedEventPositionKey>(room_id, position).await
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
        self.get_items_by_partial_key::<Event, IndexedEventRelationKey, _>(
            room_id,
            related_event_id,
        )
        .await
    }
}
