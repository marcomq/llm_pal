//! How many threads the models should use: one per fast core. On phones with big and little cores, threads on the
//! little cores slow a parallel step down, since it waits for its slowest part.

use std::sync::OnceLock;

/// Most threads a model gets, also on desktops with many cores.
const MAX_THREADS: usize = 4;

/// Cores with at least a third of the strongest core's capacity (Linux/Android `cpu_capacity`), at most
/// `MAX_THREADS`; without that information all cores, at most `MAX_THREADS`.
pub fn fast_cores() -> usize {
    static FAST: OnceLock<usize> = OnceLock::new();
    *FAST.get_or_init(|| {
        let all = std::thread::available_parallelism().map_or(MAX_THREADS, |n| n.get());
        count_fast(&capacities()).unwrap_or(all).clamp(1, MAX_THREADS)
    })
}

/// Threads for the LLM: only the strongest cores (≥ 80 % of the strongest, at least 2), at most `fast_cores()`. On the
/// Pixel 6a 2 threads make 116–132 ms per token, 4 threads (also on the A76) 165–197 ms.
pub fn llm_threads() -> usize {
    static BIG: OnceLock<usize> = OnceLock::new();
    *BIG.get_or_init(|| {
        let capacities = capacities();
        count_big(&capacities).map_or(fast_cores(), |big| big.max(2).min(fast_cores()))
    })
}

fn capacities() -> Vec<u32> {
    let all = std::thread::available_parallelism().map_or(MAX_THREADS, |n| n.get());
    (0..all)
        .filter_map(|cpu| std::fs::read_to_string(format!("/sys/devices/system/cpu/cpu{cpu}/cpu_capacity")).ok())
        .filter_map(|text| text.trim().parse().ok())
        .collect()
}

fn count_big(capacities: &[u32]) -> Option<usize> {
    let strongest = *capacities.iter().max()?;
    Some(capacities.iter().filter(|&&c| c * 5 >= strongest * 4).count())
}

fn count_fast(capacities: &[u32]) -> Option<usize> {
    let strongest = *capacities.iter().max()?;
    Some(capacities.iter().filter(|&&c| c * 3 >= strongest).count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn little_cores_do_not_count() {
        assert_eq!(count_fast(&[160, 160, 160, 160, 498, 498, 1024, 1024]), Some(4), "Pixel 6a");
        assert_eq!(count_fast(&[250, 250, 250, 250, 250, 250, 1024, 1024]), Some(2));
        assert_eq!(count_fast(&[1024; 8]), Some(8));
        assert_eq!(count_fast(&[]), None);
    }

    #[test]
    fn llm_uses_only_the_big_cores() {
        assert_eq!(count_big(&[160, 160, 160, 160, 498, 498, 1024, 1024]), Some(2), "Pixel 6a");
        assert_eq!(count_big(&[325, 325, 325, 325, 871, 871, 871, 1024]), Some(4));
        assert_eq!(count_big(&[1024; 8]), Some(8));
        assert_eq!(count_big(&[]), None);
    }
}
