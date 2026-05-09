use batpak::store::projection::{NoCache, ProjectionCache};

pub fn run() -> Result<bool, Box<dyn std::error::Error>> {
    let cache = NoCache;
    Ok(cache.get(b"projection:key")?.is_none())
}
