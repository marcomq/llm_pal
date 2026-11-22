//! Checks a voice of one's own before it is kept or used, by the rules of Pocket TTS's `import_state`.

use std::path::Path;

use safetensors::{Dtype, SafeTensors};

const MAX_BYTES: u64 = 64 * 1024 * 1024;
const LAYERS: usize = 6;
const HEADS: usize = 16;
const HEAD_DIM: usize = 64;
const NOT_A_VOICE: &str = "not a Pocket TTS voice";

/// What is wrong with the voice file at `path`, if anything.
pub fn check(path: &Path) -> Result<(), String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_BYTES {
        return Err("the file is too large".into());
    }
    check_bytes(&std::fs::read(path).map_err(|e| e.to_string())?)
}

fn check_bytes(bytes: &[u8]) -> Result<(), String> {
    let file = SafeTensors::deserialize(bytes).map_err(|_| NOT_A_VOICE)?;
    for layer in 0..LAYERS {
        let module = format!("transformer.layers.{layer}.self_attn");
        let cache = file.tensor(&format!("{module}/cache")).map_err(|_| NOT_A_VOICE)?;
        let &[2, 1, positions, HEADS, HEAD_DIM] = cache.shape() else { return Err(NOT_A_VOICE.into()) };
        if cache.dtype() != Dtype::F32 {
            return Err(NOT_A_VOICE.into());
        }
        let used = if let Ok(offset) = file.tensor(&format!("{module}/offset")) {
            let first = offset.data().first_chunk::<8>().filter(|_| offset.dtype() == Dtype::I64);
            usize::try_from(i64::from_le_bytes(*first.ok_or(NOT_A_VOICE)?)).map_err(|_| NOT_A_VOICE)?
        } else if let Ok(end) = file.tensor(&format!("{module}/current_end")) {
            end.shape().first().copied().unwrap_or(0)
        } else {
            positions
        };
        if used == 0 || used > positions {
            return Err(NOT_A_VOICE.into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use safetensors::tensor::TensorView;

    fn voice(layers: usize, heads: usize, offset: i64) -> Vec<u8> {
        let shape = vec![2, 1, 2, heads, HEAD_DIM];
        let cache = vec![0u8; shape.iter().product::<usize>() * 4];
        let offset = offset.to_le_bytes();
        let mut views = Vec::new();
        for i in 0..layers {
            let module = format!("transformer.layers.{i}.self_attn");
            views.push((format!("{module}/cache"), TensorView::new(Dtype::F32, shape.clone(), &cache).unwrap()));
            views.push((format!("{module}/offset"), TensorView::new(Dtype::I64, vec![1], &offset).unwrap()));
        }
        safetensors::serialize(views, None).unwrap()
    }

    #[test]
    fn a_voice_of_this_model_passes() {
        assert_eq!(check_bytes(&voice(LAYERS, HEADS, 2)), Ok(()));
    }

    #[test]
    fn anything_else_is_refused() {
        for bytes in [
            b"no voice".to_vec(),
            voice(LAYERS - 1, HEADS, 2),
            voice(LAYERS, HEADS / 2, 2),
            voice(LAYERS, HEADS, 0),
            voice(LAYERS, HEADS, 3),
            voice(LAYERS, HEADS, -1),
        ] {
            assert_eq!(check_bytes(&bytes), Err(NOT_A_VOICE.to_string()));
        }
    }
}
