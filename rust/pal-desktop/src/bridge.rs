//! Publishes segments as mq-bridge messages: payload = segment JSON, metadata = ids, times and WAV path.

use std::path::Path;

use anyhow::Result;
use mq_bridge::{CanonicalMessage, Publisher};
use pal_core::Segment;

pub struct Bridge {
    rt: tokio::runtime::Runtime,
    publisher: Publisher,
}

impl Bridge {
    /// `endpoint` is an mq-bridge endpoint keyed by type, e.g. `{"file": {"path": "out/segments.jsonl"}}`.
    pub fn new(endpoint: &str) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let publisher = rt.block_on(Publisher::from_config(serde_json::from_str(endpoint)?))?;
        Ok(Self { rt, publisher })
    }

    pub fn publish(&self, seg: &Segment, wav: &Path) -> Result<()> {
        self.rt.block_on(self.publisher.send(message(seg, wav)?))?;
        Ok(())
    }
}

pub fn message(seg: &Segment, wav: &Path) -> Result<CanonicalMessage> {
    Ok(CanonicalMessage::from_type(seg)?
        .with_metadata_kv("segment_id", seg.id.to_string())
        .with_metadata_kv("start_ms", seg.start_ms.to_string())
        .with_metadata_kv("end_ms", seg.end_ms.to_string())
        .with_metadata_kv("sample_rate", "16000")
        .with_metadata_kv("wav", wav.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mq_bridge::models::MemoryConfig;
    use pal_core::Features;

    fn segment(id: u64) -> Segment {
        Segment {
            id,
            start_ms: id * 1000,
            end_ms: id * 1000 + 500,
            forced_split: false,
            features: Features::default(),
            pcm: vec![0.0; 8000],
        }
    }

    #[test]
    fn segments_arrive_on_a_memory_endpoint_with_metadata() {
        let bridge = Bridge::new(r#"{"memory": {"topic": "pal-test-segments", "capacity": 10}}"#).unwrap();
        for id in 1..=3 {
            bridge.publish(&segment(id), Path::new(&format!("out/seg_{id:04}.wav"))).unwrap();
        }
        let msgs = mq_bridge::get_or_create_channel(&MemoryConfig::new("pal-test-segments", Some(10))).drain_messages();
        assert_eq!(msgs.len(), 3);
        for (i, m) in msgs.iter().enumerate() {
            let id = i as u64 + 1;
            assert_eq!(m.metadata["segment_id"], id.to_string());
            assert_eq!(m.metadata["end_ms"], (id * 1000 + 500).to_string());
            assert_eq!(m.metadata["wav"], format!("out/seg_{id:04}.wav"));
            let body: serde_json::Value = serde_json::from_slice(&m.payload).unwrap();
            assert_eq!(body["id"], id);
            assert!(body.get("pcm").is_none(), "PCM stays out of the JSON");
        }
    }
}
