//! Fixed object-to-shard routing for the first OMS milestone.

#![no_std]

use oms_types::{ObjectId, OmsError, ShardId};

#[derive(Debug, Clone)]
pub struct FixedDirectory {
    shard_count: u32,
}

impl FixedDirectory {
    /// Creates a directory with a fixed number of shards.
    ///
    /// # Errors
    ///
    /// Returns [`OmsError::InvalidShardCount`] when `shard_count` is zero.
    pub fn new(shard_count: u32) -> Result<Self, OmsError> {
        if shard_count == 0 {
            return Err(OmsError::InvalidShardCount);
        }
        Ok(Self { shard_count })
    }

    #[must_use]
    pub const fn shard_count(&self) -> u32 {
        self.shard_count
    }

    #[must_use]
    pub fn locate(&self, id: ObjectId) -> ShardId {
        let value = id.as_u128();
        let mixed = value ^ (value >> 64) ^ (value >> 32);
        let bytes = mixed.to_le_bytes();
        let hash = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        ShardId::new(hash % self.shard_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_shards() {
        assert_eq!(
            FixedDirectory::new(0).unwrap_err(),
            OmsError::InvalidShardCount
        );
    }

    #[test]
    fn route_is_stable() {
        let directory = FixedDirectory::new(8).unwrap();
        let id = ObjectId::from_u128(42);
        assert_eq!(directory.locate(id), directory.locate(id));
        assert!(directory.locate(id).get() < 8);
    }
}
