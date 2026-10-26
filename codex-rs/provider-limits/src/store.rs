use crate::ProviderLimits;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

/// File-backed limits, one `<provider_id>.json` per provider.
#[derive(Debug, Clone)]
pub struct LimitsStore {
    dir: PathBuf,
}

impl LimitsStore {
    /// Store rooted at `$CODEX_HOME/limits`.
    pub fn for_codex_home(codex_home: &Path) -> Self {
        Self::new(codex_home.join("limits"))
    }

    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn load(&self, provider_id: &str) -> Option<ProviderLimits> {
        let path = self.path_for(provider_id)?;
        let contents = fs::read(&path).ok()?;
        match serde_json::from_slice(&contents) {
            Ok(limits) => Some(limits),
            Err(err) => {
                tracing::warn!("ignoring unreadable limits file {}: {err}", path.display());
                None
            }
        }
    }

    /// All readable entries, sorted by provider id.
    pub fn load_all(&self) -> Vec<ProviderLimits> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut all: Vec<ProviderLimits> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name();
                let provider_id = name.to_str()?.strip_suffix(".json")?;
                self.load(provider_id)
            })
            .collect();
        all.sort_by(|a, b| a.provider_id.cmp(&b.provider_id));
        all
    }

    pub fn save(&self, limits: &ProviderLimits) -> io::Result<()> {
        let path = self.path_for(&limits.provider_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid provider id {:?}", limits.provider_id),
            )
        })?;
        fs::create_dir_all(&self.dir)?;
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        fs::write(&tmp, serde_json::to_vec_pretty(limits)?)?;
        fs::rename(&tmp, &path)
    }

    /// Loads (or creates) a provider's entry, applies `update`, and saves it.
    pub fn update(
        &self,
        provider_id: &str,
        display_name: &str,
        now: i64,
        update: impl FnOnce(&mut ProviderLimits),
    ) -> io::Result<ProviderLimits> {
        let mut limits = self
            .load(provider_id)
            .unwrap_or_else(|| ProviderLimits::new(provider_id, display_name, now));
        display_name.clone_into(&mut limits.display_name);
        update(&mut limits);
        limits.updated_at = now;
        self.save(&limits)?;
        Ok(limits)
    }

    fn path_for(&self, provider_id: &str) -> Option<PathBuf> {
        let valid = !provider_id.is_empty()
            && provider_id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
        valid.then(|| self.dir.join(format!("{provider_id}.json")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LimitQuality;
    use pretty_assertions::assert_eq;

    #[test]
    fn update_round_trips_and_lists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LimitsStore::new(dir.path().join("limits"));

        store
            .update("xai", "Grok", 10, |limits| {
                limits.quality = LimitQuality::Exact;
                limits.record_request(500, "2026-10-26");
            })
            .expect("save xai");
        store
            .update("deepseek", "DeepSeek", 20, |limits| {
                limits.balance = Some("$8.40".to_string());
            })
            .expect("save deepseek");

        let all = store.load_all();
        assert_eq!(
            all.iter()
                .map(|limits| limits.provider_id.as_str())
                .collect::<Vec<_>>(),
            vec!["deepseek", "xai"]
        );
        let xai = store.load("xai").expect("xai saved");
        assert_eq!(xai.quality, LimitQuality::Exact);
        assert_eq!(xai.tokens_today, 500);
        assert_eq!(xai.updated_at, 10);
    }

    #[test]
    fn rejects_path_like_provider_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LimitsStore::new(dir.path().to_path_buf());
        let limits = ProviderLimits::new("../escape", "Bad", 0);
        assert!(store.save(&limits).is_err());
        assert_eq!(store.load("../escape"), None);
    }
}
