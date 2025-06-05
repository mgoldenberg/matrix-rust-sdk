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

use serde::{Deserialize, Serialize};

use crate::serializer::MaybeEncrypted;

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

#[derive(Debug, Serialize, Deserialize)]
pub struct IndexedEvent {
    pub id: String,
    pub position: Option<String>,
    pub relation: Option<String>,
    pub content: MaybeEncrypted,
}
