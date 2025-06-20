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

pub mod types;

use gloo_utils::format::JsValueSerdeExt;
use ruma::RoomId;
use serde::{de::DeserializeOwned, Serialize};
use wasm_bindgen::JsValue;
use web_sys::IdbKeyRange;

use crate::{
    event_cache_store::{
        serializer::types::{
            Indexed, IndexedEventPositionKey, IndexedKey, IndexedKeyBounds, IndexedPartialKeyBounds,
        },
        types::{Event, Position},
    },
    serializer::IndexeddbSerializer,
    IndexeddbEventCacheStoreError,
};

#[derive(Debug)]
pub struct IndexeddbEventCacheStoreSerializer {
    inner: IndexeddbSerializer,
}

impl IndexeddbEventCacheStoreSerializer {
    pub fn new(inner: IndexeddbSerializer) -> Self {
        Self { inner }
    }

    pub fn serialize_value(
        &self,
        value: &impl Serialize,
    ) -> Result<JsValue, IndexeddbEventCacheStoreError> {
        self.inner.serialize_value(value).map_err(Into::into)
    }

    /// Decode a value that was previously encoded with
    /// [`Self::serialize_value`].
    pub fn deserialize_value<T: DeserializeOwned>(
        &self,
        value: JsValue,
    ) -> Result<T, IndexeddbEventCacheStoreError> {
        self.inner.deserialize_value(value).map_err(Into::into)
    }

    pub fn serialize<T>(
        &self,
        room_id: &RoomId,
        t: &T,
    ) -> Result<JsValue, IndexeddbEventCacheStoreError>
    where
        T: Indexed,
        T::IndexedType: Serialize,
        T::Error: Into<IndexeddbEventCacheStoreError>,
    {
        let indexed = t.to_indexed(room_id, &self.inner).map_err(Into::into)?;
        serde_wasm_bindgen::to_value(&indexed).map_err(Into::into)
    }

    pub fn deserialize<T>(&self, value: JsValue) -> Result<T, IndexeddbEventCacheStoreError>
    where
        T: Indexed,
        T::IndexedType: DeserializeOwned,
        T::Error: Into<IndexeddbEventCacheStoreError>,
    {
        let indexed: T::IndexedType = value.into_serde()?;
        T::from_indexed(indexed, &self.inner).map_err(Into::into)
    }

    pub fn encode_key<T, K>(&self, room_id: &RoomId, components: &K::KeyComponents) -> K
    where
        T: Indexed,
        K: IndexedKey<T>,
    {
        K::encode(room_id, components, &self.inner)
    }

    pub fn encode_key_as_js_value<T, K>(
        &self,
        room_id: &RoomId,
        components: &K::KeyComponents,
    ) -> Result<JsValue, serde_wasm_bindgen::Error>
    where
        T: Indexed,
        K: IndexedKey<T> + Serialize,
    {
        serde_wasm_bindgen::to_value(&self.encode_key::<T, K>(room_id, components))
    }

    pub fn encode_key_range<T, K>(
        &self,
        room_id: &RoomId,
    ) -> Result<IdbKeyRange, serde_wasm_bindgen::Error>
    where
        T: Indexed,
        K: IndexedKeyBounds<T> + Serialize,
    {
        let lower = serde_wasm_bindgen::to_value(&K::encode_lower(room_id, &self.inner))?;
        let upper = serde_wasm_bindgen::to_value(&K::encode_upper(room_id, &self.inner))?;
        Ok(IdbKeyRange::bound(&lower, &upper).expect("construct key range"))
    }

    pub fn encode_key_range_from_to<T, K>(
        &self,
        room_id: &RoomId,
        lower: &K::KeyComponents,
        upper: &K::KeyComponents,
    ) -> Result<IdbKeyRange, serde_wasm_bindgen::Error>
    where
        T: Indexed,
        K: IndexedKeyBounds<T> + Serialize,
    {
        let lower = serde_wasm_bindgen::to_value(&K::encode(room_id, lower, &self.inner))?;
        let upper = serde_wasm_bindgen::to_value(&K::encode(room_id, upper, &self.inner))?;
        Ok(IdbKeyRange::bound(&lower, &upper)?)
    }

    pub fn encode_partial_key_range<T, K, C>(
        &self,
        room_id: &RoomId,
        components: &C,
    ) -> Result<IdbKeyRange, serde_wasm_bindgen::Error>
    where
        T: Indexed,
        K: IndexedPartialKeyBounds<T, C> + Serialize,
    {
        let lower = serde_wasm_bindgen::to_value(&K::encode_partial_lower(
            room_id,
            components,
            &self.inner,
        ))?;
        let upper = serde_wasm_bindgen::to_value(&K::encode_partial_upper(
            room_id,
            components,
            &self.inner,
        ))?;
        Ok(IdbKeyRange::bound(&lower, &upper)?)
    }

    pub fn encode_event_position_range_for_chunk(
        &self,
        room_id: &RoomId,
        chunk_id: u64,
    ) -> Result<IdbKeyRange, serde_wasm_bindgen::Error> {
        self.encode_key_range_from_to::<Event, IndexedEventPositionKey>(
            room_id,
            &Position { chunk_identifier: chunk_id, index: 0 },
            &Position {
                chunk_identifier: chunk_id,
                index: js_sys::Number::MAX_SAFE_INTEGER as usize,
            },
        )
    }

    pub fn encode_event_position_range_for_chunk_from(
        &self,
        room_id: &RoomId,
        position: &Position,
    ) -> Result<IdbKeyRange, IndexeddbEventCacheStoreError> {
        Ok(self.encode_key_range_from_to::<Event, IndexedEventPositionKey>(
            room_id,
            position,
            &Position {
                chunk_identifier: position.chunk_identifier,
                index: js_sys::Number::MAX_SAFE_INTEGER as usize,
            },
        )?)
    }
}
