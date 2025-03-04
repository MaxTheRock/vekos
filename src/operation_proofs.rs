/*
* Copyright 2023-2024 Juan Miguel Giraldo
*
* Licensed under the Apache License, Version 2.0 (the "License");
* you may not use this file except in compliance with the License.
* You may obtain a copy of the License at
*
*     http://www.apache.org/licenses/LICENSE-2.0
*
* Unless required by applicable law or agreed to in writing, software
* distributed under the License is distributed on an "AS IS" BASIS,
* WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
* See the License for the specific language governing permissions and
* limitations under the License.
*/

use crate::{
    hash, tsc,
    verification::{Hash, OperationProof, Verifiable, VerificationError},
    vkfs::Superblock,
};
use x86_64::VirtAddr;
use crate::verification::FSOpType;
use crate::verification::ProofData;
use alloc::vec::Vec;

#[derive(Debug, Clone)]
pub struct BlockOperationProof {
    pub block_num: u64,
    pub block_hash: Hash,
    pub prev_state: Hash,
    pub new_state: Hash,
    pub timestamp: u64,
}

impl BlockOperationProof {
    pub fn new(block_num: u64, block_data: &[u8], prev_state: Hash) -> Self {
        let block_hash =
            hash::hash_memory(VirtAddr::new(block_data.as_ptr() as u64), block_data.len());
        let new_state = Hash(prev_state.0 ^ block_hash.0);

        Self {
            block_num,
            block_hash,
            prev_state,
            new_state,
            timestamp: tsc::read_tsc(),
        }
    }

    pub fn verify(&self, block_data: &[u8]) -> bool {
        let current_hash =
            hash::hash_memory(VirtAddr::new(block_data.as_ptr() as u64), block_data.len());
        current_hash == self.block_hash
    }
}

pub trait ProofVerifier {
    fn verify_operation_proof(&self, proof: &OperationProof) -> Result<bool, VerificationError>;
    fn verify_block_proof(&self, proof: &BlockOperationProof) -> Result<bool, VerificationError>;
    fn generate_block_proof(
        &self,
        block_num: u64,
    ) -> Result<BlockOperationProof, VerificationError>;
    fn verify_hash_chain(&self, proof: &OperationProof) -> Result<bool, VerificationError>;
}

impl ProofVerifier for Superblock {
    fn verify_operation_proof(&self, proof: &OperationProof) -> Result<bool, VerificationError> {
        let current_state = self.state_hash();
        if proof.prev_state != current_state {
            return Ok(false);
        }

        if !verify_signature(proof) {
            return Ok(false);
        }

        match &proof.data {
            ProofData::Filesystem(fs_proof) => {
                let computed_hash = match fs_proof.operation {
                    FSOpType::Create | FSOpType::Modify => hash::hash_memory(
                        VirtAddr::new(fs_proof.path.as_ptr() as u64),
                        fs_proof.path.len(),
                    ),
                    FSOpType::Delete => Hash(!current_state.0),
                };

                if computed_hash != fs_proof.content_hash {
                    return Ok(false);
                }

                let expected_state = Hash(proof.prev_state.0 ^ computed_hash.0);
                if expected_state != proof.new_state {
                    return Ok(false);
                }

                Ok(true)
            }
            _ => Err(VerificationError::InvalidProof),
        }
    }

    fn verify_hash_chain(&self, proof: &OperationProof) -> Result<bool, VerificationError> {
        match &proof.data {
            ProofData::Filesystem(fs_proof) => {
                let current_hash = fs_proof.prev_state;

                let operation_hash = match fs_proof.operation {
                    FSOpType::Create | FSOpType::Modify => fs_proof.content_hash,
                    FSOpType::Delete => Hash(!current_hash.0),
                };

                let expected_hash = Hash(current_hash.0 ^ operation_hash.0);
                if expected_hash != fs_proof.new_state {
                    return Ok(false);
                }

                Ok(true)
            }
            _ => Err(VerificationError::InvalidProof),
        }
    }

    fn verify_block_proof(&self, proof: &BlockOperationProof) -> Result<bool, VerificationError> {
        let block_data = self
            .block_cache
            .lock()
            .get_block(proof.block_num)
            .ok_or(VerificationError::InvalidState)?;

        if !proof.verify(&block_data) {
            return Ok(false);
        }

        let current_state = self.state_hash();
        if proof.prev_state != current_state {
            return Ok(false);
        }

        let expected_state = Hash(current_state.0 ^ proof.block_hash.0);
        if expected_state != proof.new_state {
            return Ok(false);
        }

        Ok(true)
    }

    fn generate_block_proof(
        &self,
        block_num: u64,
    ) -> Result<BlockOperationProof, VerificationError> {
        let block_data = self
            .block_cache
            .lock()
            .get_block(block_num)
            .ok_or(VerificationError::InvalidState)?;

        Ok(BlockOperationProof::new(
            block_num,
            &block_data,
            self.state_hash(),
        ))
    }
}

fn verify_signature(proof: &OperationProof) -> bool {
    let mut verification_data = Vec::new();
    verification_data.extend_from_slice(&proof.op_id.to_ne_bytes());
    verification_data.extend_from_slice(&proof.prev_state.0.to_ne_bytes());
    verification_data.extend_from_slice(&proof.new_state.0.to_ne_bytes());

    match &proof.data {
        ProofData::Memory(mem_proof) => {
            verification_data.extend_from_slice(&[0]);
            verification_data.extend_from_slice(&(mem_proof.address.as_u64().to_ne_bytes()));
            verification_data.extend_from_slice(&(mem_proof.size.to_ne_bytes()));
            verification_data.extend_from_slice(&(mem_proof.frame_hash.0.to_ne_bytes()));
        },
        ProofData::Filesystem(fs_proof) => {
            verification_data.extend_from_slice(&[1]);
            verification_data.extend_from_slice(fs_proof.path.as_bytes());
            verification_data.extend_from_slice(&fs_proof.content_hash.0.to_ne_bytes());
        },
        ProofData::Process(proc_proof) => {
            verification_data.extend_from_slice(&[2]);
            verification_data.extend_from_slice(&proc_proof.pid.to_ne_bytes());
            verification_data.extend_from_slice(&proc_proof.state_hash.0.to_ne_bytes());
        },
        ProofData::Boot(boot_proof) => {
            verification_data.extend_from_slice(&[3]);
            verification_data.extend_from_slice(&boot_proof.stage_hash.0.to_ne_bytes());
        },
        ProofData::Tile(tile_proof) => {
            verification_data.extend_from_slice(&[4]);
            verification_data.extend_from_slice(&tile_proof.tile_id.to_ne_bytes());
            verification_data.extend_from_slice(&(tile_proof.position.0.to_ne_bytes()));
            verification_data.extend_from_slice(&(tile_proof.position.1.to_ne_bytes()));
            verification_data.extend_from_slice(&tile_proof.tile_hash.0.to_ne_bytes());
        },
        ProofData::Generic { operation_type, data_hash } => {
            verification_data.extend_from_slice(&[5]);
            verification_data.extend_from_slice(operation_type.as_bytes());
            verification_data.extend_from_slice(&data_hash.0.to_ne_bytes());
        },
    }

    let verifier = crate::crypto::CRYPTO_VERIFIER.lock();
    verifier.verify_signature(&verification_data, &proof.signature)
}
