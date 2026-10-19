use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::Read,
    path::Path,
    sync::Mutex,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const SWARM_CHUNK_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRecord {
    pub index: u64,
    pub length: u32,
    pub blake3: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkManifest {
    pub file_size: u64,
    pub chunks: Vec<ChunkRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerState {
    Discovered,
    Connected,
    Receiving,
    Complete,
}

#[derive(Debug, Clone)]
pub struct PeerRecord {
    pub state: PeerState,
    pub assigned: HashSet<u64>,
}

pub struct SwarmCoordinator {
    peers: Mutex<HashMap<String, PeerRecord>>,
}

impl SwarmCoordinator {
    pub fn new() -> Self {
        Self {
            peers: Mutex::new(HashMap::new()),
        }
    }

    pub fn register_peer(&self, peer_id: String) {
        self.peers.lock().expect("peer registry poisoned").insert(
            peer_id,
            PeerRecord {
                state: PeerState::Connected,
                assigned: HashSet::new(),
            },
        );
    }

    pub fn assign_next(&self, peer_id: &str, manifest: &ChunkManifest) -> Option<u64> {
        let mut peers = self.peers.lock().ok()?;
        let already_assigned: HashSet<u64> = peers
            .values()
            .flat_map(|registered| registered.assigned.iter().copied())
            .collect();
        let next = manifest
            .chunks
            .iter()
            .map(|chunk| chunk.index)
            .find(|index| !already_assigned.contains(index));
        let peer = peers.get_mut(peer_id)?;
        if let Some(index) = next {
            peer.assigned.insert(index);
            peer.state = PeerState::Receiving;
        }
        next
    }
}

impl Default for SwarmCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

pub fn build_manifest(path: &Path) -> Result<ChunkManifest> {
    let mut file = File::open(path).context("open file for manifest")?;
    let file_size = file.metadata()?.len();
    let mut buffer = vec![0_u8; SWARM_CHUNK_BYTES];
    let mut chunks = Vec::new();
    let mut index = 0_u64;
    loop {
        let mut read = 0;
        while read < buffer.len() {
            let amount = file.read(&mut buffer[read..])?;
            if amount == 0 {
                break;
            }
            read += amount;
        }
        if read == 0 {
            break;
        }
        chunks.push(ChunkRecord {
            index,
            length: read as u32,
            blake3: blake3::hash(&buffer[..read]).to_hex().to_string(),
        });
        index += 1;
    }
    Ok(ChunkManifest { file_size, chunks })
}
