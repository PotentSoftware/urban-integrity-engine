use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use urban_integrity_schema::types::id::EntityId;

/// Buffers partial evidence of a motif for a short time.
/// Used in Tier A checks when we see an Officer but need the
/// corresponding Contract to complete the motif.
pub struct PendingEvidenceBuffer {
    /// Maps EntityId -> (Instant, EvidenceData)
    buffer: Mutex<HashMap<EntityId, (Instant, String)>>,
    ttl: Duration,
}

impl PendingEvidenceBuffer {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            buffer: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    pub fn insert(&self, id: EntityId, evidence: String) {
        let mut buf = self.buffer.lock().unwrap();
        buf.insert(id, (Instant::now(), evidence));

        // Naive cleanup on insert
        let now = Instant::now();
        let ttl = self.ttl;
        buf.retain(|_, (time, _)| now.duration_since(*time) < ttl);
    }

    pub fn pop(&self, id: &EntityId) -> Option<String> {
        let mut buf = self.buffer.lock().unwrap();
        buf.remove(id).map(|(_, ev)| ev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_buffer_insert_pop() {
        let buffer = PendingEvidenceBuffer::new(60);
        let id = EntityId("test".to_string());
        buffer.insert(id.clone(), "evidence_a".to_string());

        let retrieved = buffer.pop(&id);
        assert_eq!(retrieved, Some("evidence_a".to_string()));

        // Already popped
        assert_eq!(buffer.pop(&id), None);
    }

    #[test]
    fn test_buffer_ttl_cleanup() {
        let buffer = PendingEvidenceBuffer::new(1);
        let id = EntityId("test".to_string());
        buffer.insert(id.clone(), "evidence_a".to_string());

        thread::sleep(Duration::from_millis(1100));

        // Trigger cleanup with another insert
        buffer.insert(EntityId("other".to_string()), "evidence_b".to_string());

        // original should be gone
        assert_eq!(buffer.pop(&id), None);
    }
}
