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

// This module contains a small hack to have the following macro invocations
// act as the appropriate trait impl block on wasm, but still be compiled on
// non-wasm as a regular impl block.
//
// The trait impl doesn't compile on non-wasm due to unfulfilled trait bounds,
// but this hack allows us to still have most of rust-analyzer's IDE
// functionality within the impl block without having to set it up to check
// things against the wasm target (which would disable many other parts of the
// codebase).

#[cfg(target_arch = "wasm32")]
macro_rules! impl_event_cache_store {
    ( $($body:tt)* ) => {
        #[async_trait::async_trait(?Send)]
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

#[cfg(target_arch = "wasm32")]
macro_rules! impl_event_cache_store_media {
    ( $($body:tt)* ) => {
        #[async_trait::async_trait(?Send)]
        impl EventCacheStoreMedia for IndexeddbEventCacheStore {
            type Error = IndexeddbEventCacheStoreError;

            $($body)*
        }
    };
}

#[cfg(not(target_arch = "wasm32"))]
macro_rules! impl_event_cache_store_media {
    ( $($body:tt)* ) => {
        impl IndexeddbEventCacheStore {
            $($body)*
        }
    };
}

pub(super) use impl_event_cache_store;
pub(super) use impl_event_cache_store_media;
