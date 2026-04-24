use criterion::measurement::WallTime;
use criterion::{BenchmarkGroup, SamplingMode, Throughput};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BenchProfile {
    Quick,
    QuickWarm,
    Heavy,
    Massive,
}

pub fn apply_profile(group: &mut BenchmarkGroup<'_, WallTime>, profile: BenchProfile) {
    group.noise_threshold(0.03);
    group.significance_level(0.05);
    match profile {
        BenchProfile::Quick => {
            group.sample_size(50);
            group.warm_up_time(Duration::from_secs(2));
            group.measurement_time(Duration::from_secs(8));
            group.sampling_mode(SamplingMode::Auto);
        }
        BenchProfile::QuickWarm => {
            group.sample_size(30);
            group.warm_up_time(Duration::from_secs(2));
            group.measurement_time(Duration::from_secs(12));
            group.sampling_mode(SamplingMode::Auto);
        }
        BenchProfile::Heavy => {
            group.sample_size(10);
            group.warm_up_time(Duration::from_secs(3));
            group.measurement_time(Duration::from_secs(30));
            group.sampling_mode(SamplingMode::Flat);
        }
        BenchProfile::Massive => {
            group.sample_size(10);
            group.warm_up_time(Duration::from_secs(3));
            group.measurement_time(Duration::from_secs(120));
            group.sampling_mode(SamplingMode::Flat);
        }
    }
}

pub fn profile_for_event_count(count: u64) -> BenchProfile {
    match count {
        0..=1_000 => BenchProfile::Quick,
        1_001..=10_000 => BenchProfile::Heavy,
        _ => BenchProfile::Massive,
    }
}

pub fn throughput_elements(group: &mut BenchmarkGroup<'_, WallTime>, elements: u64) {
    group.throughput(Throughput::Elements(elements));
}
