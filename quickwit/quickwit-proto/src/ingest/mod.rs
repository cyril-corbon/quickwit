// Copyright (C) 2024 Quickwit, Inc.
//
// Quickwit is offered under the AGPL v3.0 and as commercial software.
// For commercial licensing, contact us at hello@quickwit.io.
//
// AGPL:
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

use bytes::Bytes;
use bytesize::ByteSize;

use self::ingester::{PersistFailureReason, ReplicateFailureReason};
use self::router::IngestFailureReason;
use super::types::NodeId;
use super::GrpcServiceError;
use crate::types::{queue_id, IndexUid, Position, QueueId, ShardId};
use crate::{ServiceError, ServiceErrorCode};

pub mod ingester;
pub mod router;

include!("../codegen/quickwit/quickwit.ingest.rs");

pub type IngestV2Result<T> = std::result::Result<T, IngestV2Error>;

#[derive(Debug, thiserror::Error, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestV2Error {
    #[error("internal error: {0}")]
    Internal(String),
    #[error("shard `{shard_id}` not found")]
    ShardNotFound { shard_id: ShardId },
    #[error("request timed out: {0}")]
    Timeout(String),
    #[error("too many requests")]
    TooManyRequests,
    #[error("service unavailable: {0}")]
    Unavailable(String),
}

impl ServiceError for IngestV2Error {
    fn error_code(&self) -> ServiceErrorCode {
        match self {
            Self::Internal(_) => ServiceErrorCode::Internal,
            Self::ShardNotFound { .. } => ServiceErrorCode::NotFound,
            Self::Timeout(_) => ServiceErrorCode::Timeout,
            Self::TooManyRequests => ServiceErrorCode::TooManyRequests,
            Self::Unavailable(_) => ServiceErrorCode::Unavailable,
        }
    }
}

impl GrpcServiceError for IngestV2Error {
    fn new_internal(message: String) -> Self {
        Self::Internal(message)
    }

    fn new_timeout(message: String) -> Self {
        Self::Timeout(message)
    }

    fn new_unavailable(message: String) -> Self {
        Self::Unavailable(message)
    }
}

impl Shard {
    /// List of nodes that are storing the shard (the leader, and optionally the follower).
    pub fn ingesters(&self) -> impl Iterator<Item = NodeId> + '_ {
        [Some(&self.leader_id), self.follower_id.as_ref()]
            .into_iter()
            .flatten()
            .map(|node_id| NodeId::new(node_id.clone()))
    }
}

impl DocBatchV2 {
    pub fn docs(self) -> impl Iterator<Item = Bytes> {
        let DocBatchV2 {
            doc_buffer,
            doc_lengths,
        } = self;
        doc_lengths
            .into_iter()
            .scan(0, move |start_offset, doc_length| {
                let start = *start_offset;
                let end = start + doc_length as usize;
                *start_offset = end;
                Some(doc_buffer.slice(start..end))
            })
    }

    pub fn is_empty(&self) -> bool {
        self.doc_lengths.is_empty()
    }

    pub fn num_bytes(&self) -> usize {
        self.doc_buffer.len()
    }

    pub fn num_docs(&self) -> usize {
        self.doc_lengths.len()
    }

    #[cfg(any(test, feature = "testsuite"))]
    pub fn for_test(docs: impl IntoIterator<Item = &'static str>) -> Self {
        let mut doc_buffer = Vec::new();
        let mut doc_lengths = Vec::new();

        for doc in docs {
            doc_buffer.extend(doc.as_bytes());
            doc_lengths.push(doc.len() as u32);
        }
        Self {
            doc_lengths,
            doc_buffer: Bytes::from(doc_buffer),
        }
    }
}

impl MRecordBatch {
    pub fn encoded_mrecords(&self) -> impl Iterator<Item = Bytes> + '_ {
        self.mrecord_lengths
            .iter()
            .scan(0, |start_offset, mrecord_length| {
                let start = *start_offset;
                let end = start + *mrecord_length as usize;
                *start_offset = end;
                Some(self.mrecord_buffer.slice(start..end))
            })
    }

    pub fn is_empty(&self) -> bool {
        self.mrecord_lengths.is_empty()
    }

    pub fn estimate_size(&self) -> ByteSize {
        ByteSize((self.mrecord_buffer.len() + self.mrecord_lengths.len() * 4) as u64)
    }

    pub fn num_mrecords(&self) -> usize {
        self.mrecord_lengths.len()
    }

    #[cfg(any(test, feature = "testsuite"))]
    pub fn for_test(mrecords: impl IntoIterator<Item = &'static str>) -> Option<Self> {
        let mut mrecord_buffer = Vec::new();
        let mut mrecord_lengths = Vec::new();

        for mrecord in mrecords {
            mrecord_buffer.extend(mrecord.as_bytes());
            mrecord_lengths.push(mrecord.len() as u32);
        }
        Some(Self {
            mrecord_lengths,
            mrecord_buffer: Bytes::from(mrecord_buffer),
        })
    }
}

impl Shard {
    pub fn shard_id(&self) -> &ShardId {
        self.shard_id
            .as_ref()
            .expect("`shard_id` should be a required field")
    }

    pub fn is_open(&self) -> bool {
        self.shard_state().is_open()
    }

    pub fn is_unavailable(&self) -> bool {
        self.shard_state().is_unavailable()
    }

    pub fn is_closed(&self) -> bool {
        self.shard_state().is_closed()
    }

    pub fn queue_id(&self) -> super::types::QueueId {
        queue_id(self.index_uid(), &self.source_id, self.shard_id())
    }

    pub fn publish_position_inclusive(&self) -> &Position {
        self.publish_position_inclusive
            .as_ref()
            .expect("`publish_position_inclusive` should be a required field")
    }
}

impl ShardState {
    pub fn is_open(&self) -> bool {
        *self == ShardState::Open
    }

    pub fn is_unavailable(&self) -> bool {
        *self == ShardState::Unavailable
    }

    pub fn is_closed(&self) -> bool {
        *self == ShardState::Closed
    }

    pub fn as_json_str_name(&self) -> &'static str {
        match self {
            ShardState::Unspecified => "unspecified",
            ShardState::Open => "open",
            ShardState::Unavailable => "unavailable",
            ShardState::Closed => "closed",
        }
    }

    pub fn from_json_str_name(shard_state_json_name: &str) -> Option<Self> {
        match shard_state_json_name {
            "unspecified" => Some(Self::Unspecified),
            "open" => Some(Self::Open),
            "unavailable" => Some(Self::Unavailable),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }
}

impl ShardIds {
    pub fn queue_ids(&self) -> impl Iterator<Item = QueueId> + '_ {
        self.shard_ids
            .iter()
            .map(|shard_id| queue_id(self.index_uid(), &self.source_id, shard_id))
    }
}

impl ShardIdPositions {
    pub fn index_uid(&self) -> &IndexUid {
        self.index_uid
            .as_ref()
            .expect("`index_uid` should be a required field")
    }

    pub fn queue_id_positions(&self) -> impl Iterator<Item = (QueueId, &Position)> + '_ {
        self.shard_positions.iter().map(|shard_position| {
            let queue_id = queue_id(self.index_uid(), &self.source_id, shard_position.shard_id());
            (queue_id, shard_position.publish_position_inclusive())
        })
    }
}

impl ShardIdPosition {
    pub fn shard_id(&self) -> &ShardId {
        self.shard_id
            .as_ref()
            .expect("`shard_id` should be a required field")
    }

    pub fn publish_position_inclusive(&self) -> &Position {
        self.publish_position_inclusive
            .as_ref()
            .expect("`publish_position_inclusive` should be a required field")
    }
}

impl From<PersistFailureReason> for IngestFailureReason {
    fn from(reason: PersistFailureReason) -> Self {
        match reason {
            PersistFailureReason::Unspecified => IngestFailureReason::Unspecified,
            PersistFailureReason::ShardNotFound => IngestFailureReason::NoShardsAvailable,
            PersistFailureReason::ShardClosed => IngestFailureReason::NoShardsAvailable,
            PersistFailureReason::ResourceExhausted => IngestFailureReason::ResourceExhausted,
            PersistFailureReason::RateLimited => IngestFailureReason::RateLimited,
        }
    }
}

impl From<ReplicateFailureReason> for PersistFailureReason {
    fn from(reason: ReplicateFailureReason) -> Self {
        match reason {
            ReplicateFailureReason::Unspecified => PersistFailureReason::Unspecified,
            ReplicateFailureReason::ShardNotFound => PersistFailureReason::ShardNotFound,
            ReplicateFailureReason::ShardClosed => PersistFailureReason::ShardClosed,
            ReplicateFailureReason::ResourceExhausted => PersistFailureReason::ResourceExhausted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shard_state_json_str_name() {
        let shard_state_json_name = ShardState::Unspecified.as_json_str_name();
        let shard_state = ShardState::from_json_str_name(shard_state_json_name).unwrap();
        assert_eq!(shard_state, ShardState::Unspecified);

        let shard_state_json_name = ShardState::Open.as_json_str_name();
        let shard_state = ShardState::from_json_str_name(shard_state_json_name).unwrap();
        assert_eq!(shard_state, ShardState::Open);

        let shard_state_json_name = ShardState::Unavailable.as_json_str_name();
        let shard_state = ShardState::from_json_str_name(shard_state_json_name).unwrap();
        assert_eq!(shard_state, ShardState::Unavailable);

        let shard_state_json_name = ShardState::Closed.as_json_str_name();
        let shard_state = ShardState::from_json_str_name(shard_state_json_name).unwrap();
        assert_eq!(shard_state, ShardState::Closed);

        assert!(ShardState::from_json_str_name("unknown").is_none());
    }
}
