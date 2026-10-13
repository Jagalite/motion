//! Process-local ownership of generated artifacts. Durable cleanup intent stays
//! in SQLite; these leases fence live readers and uncancellable I/O until drop.
use playscale_core::maintenance::{ArtifactUse, ArtifactUseInput};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Default)]
pub struct Registry(Mutex<BTreeMap<String, ArtifactUse>>);

pub struct Lease {
    registry: Arc<Registry>,
    files: Vec<String>,
    release: ArtifactUseInput,
}

impl Registry {
    pub fn pin(self: &Arc<Self>, files: impl IntoIterator<Item = String>) -> Option<Arc<Lease>> {
        let mut files: Vec<_> = files.into_iter().collect();
        files.sort();
        files.dedup();
        self.acquire(files, ArtifactUseInput::Pin, ArtifactUseInput::ReleasePin)
    }

    pub fn claim_cleanup(self: &Arc<Self>, file: String) -> Option<Arc<Lease>> {
        self.acquire(
            vec![file],
            ArtifactUseInput::ClaimCleanup,
            ArtifactUseInput::ReleaseCleanup,
        )
    }

    fn acquire(
        self: &Arc<Self>,
        files: Vec<String>,
        input: ArtifactUseInput,
        release: ArtifactUseInput,
    ) -> Option<Arc<Lease>> {
        let mut entries = self.0.lock().unwrap();
        let next = files
            .iter()
            .map(|file| {
                entries
                    .get(file)
                    .copied()
                    .unwrap_or_default()
                    .transition(input)
            })
            .collect::<Option<Vec<_>>>()?;
        for (file, state) in files.iter().zip(next) {
            entries.insert(file.clone(), state);
        }
        Some(Arc::new(Lease {
            registry: self.clone(),
            files,
            release,
        }))
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut entries = self.registry.0.lock().unwrap();
        for file in &self.files {
            let state = entries[file]
                .transition(self.release)
                .expect("owned artifact lease");
            if state == ArtifactUse::default() {
                entries.remove(file);
            } else {
                entries.insert(file.clone(), state);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multipart_admission_is_atomic_and_clones_hold_ownership() {
        let registry = Arc::new(Registry::default());
        let cleanup = registry.claim_cleanup("b".into()).unwrap();
        assert!(registry.pin(["a".into(), "b".into()]).is_none());
        assert!(registry.claim_cleanup("a".into()).is_some());
        drop(cleanup);
        let pin = registry.pin(["a".into(), "b".into(), "a".into()]).unwrap();
        let reader = pin.clone();
        drop(pin);
        assert!(registry.claim_cleanup("a".into()).is_none());
        assert!(registry.claim_cleanup("b".into()).is_none());
        drop(reader);
        assert!(registry.claim_cleanup("a".into()).is_some());
        assert!(registry.claim_cleanup("b".into()).is_some());
        assert!(registry.0.lock().unwrap().is_empty());
    }
}
