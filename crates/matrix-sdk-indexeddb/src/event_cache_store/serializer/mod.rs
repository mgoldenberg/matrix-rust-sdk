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
use thiserror::Error;
use wasm_bindgen::JsValue;
use web_sys::IdbKeyRange;

use crate::{
    event_cache_store::serializer::types::{
        Indexed, IndexedKey, IndexedKeyBounds, IndexedKeyRange,
    },
    serializer::{IndexeddbSerializer, IndexeddbSerializerError},
};

#[derive(Debug, Error)]
pub enum IndexeddbEventCacheStoreSerializerError<IndexingError> {
    #[error("indexing: {0}")]
    Indexing(IndexingError),
    #[error("serialization: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl<T> From<serde_wasm_bindgen::Error> for IndexeddbEventCacheStoreSerializerError<T> {
    fn from(e: serde_wasm_bindgen::Error) -> Self {
        Self::Serialization(serde::de::Error::custom(e.to_string()))
    }
}

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
    ) -> Result<JsValue, IndexeddbEventCacheStoreSerializerError<IndexeddbSerializerError>> {
        self.inner.serialize_value(value).map_err(IndexeddbEventCacheStoreSerializerError::Indexing)
    }

    /// Decode a value that was previously encoded with
    /// [`Self::serialize_value`].
    pub fn deserialize_value<T: DeserializeOwned>(
        &self,
        value: JsValue,
    ) -> Result<T, IndexeddbEventCacheStoreSerializerError<IndexeddbSerializerError>> {
        self.inner
            .deserialize_value(value)
            .map_err(IndexeddbEventCacheStoreSerializerError::Indexing)
    }

    pub fn serialize<T>(
        &self,
        room_id: &RoomId,
        t: &T,
    ) -> Result<JsValue, IndexeddbEventCacheStoreSerializerError<T::Error>>
    where
        T: Indexed,
        T::IndexedType: Serialize,
    {
        let indexed = t
            .to_indexed(room_id, &self.inner)
            .map_err(IndexeddbEventCacheStoreSerializerError::Indexing)?;
        serde_wasm_bindgen::to_value(&indexed).map_err(Into::into)
    }

    pub fn deserialize<T>(
        &self,
        value: JsValue,
    ) -> Result<T, IndexeddbEventCacheStoreSerializerError<T::Error>>
    where
        T: Indexed,
        T::IndexedType: DeserializeOwned,
    {
        let indexed: T::IndexedType = value.into_serde()?;
        T::from_indexed(indexed, &self.inner)
            .map_err(IndexeddbEventCacheStoreSerializerError::Indexing)
    }

    pub fn encode_key<T, K>(&self, room_id: &RoomId, components: &K::KeyComponents) -> K
    where
        T: Indexed,
        K: IndexedKey<T>,
    {
        K::encode(room_id, components, &self.inner)
    }

    pub fn encode_key_as_value<T, K>(
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

    pub fn encode_key_range<'a, T, K>(
        &self,
        room_id: &RoomId,
        range: impl Into<IndexedKeyRange<&'a K::KeyComponents>>,
    ) -> Result<IdbKeyRange, serde_wasm_bindgen::Error>
    where
        T: Indexed,
        K: IndexedKeyBounds<T> + Serialize,
        K::KeyComponents: 'a,
    {
        Ok(match range.into() {
            IndexedKeyRange::Only(key) => {
                let key = serde_wasm_bindgen::to_value(&K::encode(room_id, key, &self.inner))?;
                IdbKeyRange::only(&key)?
            }
            IndexedKeyRange::Bound(lower, upper) => {
                let lower = serde_wasm_bindgen::to_value(&K::encode(room_id, lower, &self.inner))?;
                let upper = serde_wasm_bindgen::to_value(&K::encode(room_id, upper, &self.inner))?;
                IdbKeyRange::bound(&lower, &upper)?
            }
            IndexedKeyRange::All => {
                let lower = serde_wasm_bindgen::to_value(&K::lower_key(room_id, &self.inner))?;
                let upper = serde_wasm_bindgen::to_value(&K::upper_key(room_id, &self.inner))?;
                IdbKeyRange::bound(&lower, &upper).expect("construct key range")
            }
        })
    }
}
