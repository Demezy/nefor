//! Request-local consumed-input evidence for project artifact caching.
//! This API always compiles cold; validation performs only resolver and file IO.
use crate::error::MagError;
use crate::profile::CompileProfiler;
use crate::{resolver, FileCompileRequest};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Query {
    kind: Operation,
    requested: String,
    roots: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Operation {
    Entry,
    Require,
    Read,
    ReadJson,
}

impl Query {
    fn new(kind: Operation, roots: Vec<PathBuf>, requested: &str) -> Self {
        Self {
            kind,
            roots,
            requested: requested.into(),
        }
    }
    pub(crate) fn entry(root: &Path, path: &str) -> Self {
        Self::new(Operation::Entry, vec![root.into()], path)
    }
    pub(crate) fn read(root: &Path, path: &str) -> Self {
        Self::new(Operation::Read, vec![root.into()], path)
    }
    pub(crate) fn module(roots: &[PathBuf], name: &str) -> Self {
        Self::new(Operation::Require, roots.to_vec(), name)
    }
    pub(crate) fn json(roots: Vec<PathBuf>, path: &str) -> Self {
        Self::new(Operation::ReadJson, roots, path)
    }
    fn resolve(&self) -> Option<PathBuf> {
        match self.kind {
            Operation::Entry | Operation::Read => match self.roots.as_slice() {
                [root] => resolver::resolve_workspace_path(root, &self.requested).ok(),
                _ => None,
            },
            Operation::Require => resolver::resolve_module(&self.roots, &self.requested)
                .ok()
                .map(|resolved| resolved.path),
            Operation::ReadJson => resolver::resolve_json(&self.roots, &self.requested).ok(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    query: Query,
    // Keep the actual memoization spelling separate from canonical target identity.
    selected: PathBuf,
    canonical: PathBuf,
    digest: [u8; 32],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationSet {
    version: u32,
    cwd: Option<PathBuf>,
    consistent: bool,
    records: Vec<Observation>,
}

impl ObservationSet {
    /// Re-run observed queries, not directory scans. Every error is a miss.
    /// This is freshness evidence, not filesystem snapshot isolation.
    pub fn validate(&self) -> bool {
        if self.version != 1 || !self.consistent || self.records.is_empty() {
            return false;
        }
        if self
            .records
            .iter()
            .any(|r| r.query.roots.iter().any(|p| p.is_relative()))
            && (self.cwd.is_none() || std::env::current_dir().ok() != self.cwd)
        {
            return false;
        }
        if self
            .records
            .iter()
            .filter(|r| r.query.kind == Operation::Entry)
            .count()
            != 1
        {
            return false;
        }
        self.records.iter().all(|record| {
            let Some(selected) = record.query.resolve() else {
                return false;
            };
            selected == record.selected
                && canonical_identity(&selected) == record.canonical
                && std::fs::read(&selected).is_ok_and(|bytes| digest(&bytes) == record.digest)
        })
    }
}

fn canonical_identity(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.into())
}
fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

#[derive(Clone, Debug)]
pub(crate) struct Observer(Arc<Mutex<ObservationSet>>);
impl Observer {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(ObservationSet {
            version: 1,
            cwd: std::env::current_dir().ok(),
            consistent: true,
            records: Vec::new(),
        })))
    }
    pub(crate) fn record(&self, query: Query, path: &Path, source: &str) {
        let record = Observation {
            query,
            selected: path.into(),
            canonical: canonical_identity(path),
            digest: digest(source.as_bytes()),
        };
        let mut set = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = set.records.iter().find(|r| r.query == record.query) {
            if previous != &record {
                set.consistent = false;
            }
        } else {
            // Aliased queries consuming contradictory buffers cannot be published.
            if set
                .records
                .iter()
                .any(|r| r.canonical == record.canonical && r.digest != record.digest)
            {
                set.consistent = false;
            }
            set.records.push(record);
        }
    }
}

pub struct ObservedCompilation {
    pub artifact: serde_json::Value,
    pub observations: ObservationSet,
}

pub fn compile_file_observed(
    request: FileCompileRequest<'_>,
    profiler: Option<&CompileProfiler>,
) -> Result<ObservedCompilation, MagError> {
    let observer = Observer::new();
    let artifact = crate::compile_file_cold_observing(request, profiler, Some(observer.clone()))?;
    let observations = observer.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Ok(ObservedCompilation {
        artifact,
        observations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_buffers_are_not_replaced_by_later_disk_contents() {
        let observer = Observer::new();
        observer.record(
            Query::entry(Path::new("."), "Cargo.toml"),
            Path::new("Cargo.toml"),
            "not the disk buffer",
        );
        assert!(!observer.0.lock().unwrap().validate());
    }

    #[test]
    fn contradictory_consumption_is_not_publishable() {
        let observer = Observer::new();
        let path = Path::new("Cargo.toml");
        observer.record(Query::entry(Path::new("."), "Cargo.toml"), path, "first");
        observer.record(Query::entry(Path::new("."), "Cargo.toml"), path, "second");
        assert!(!observer.0.lock().unwrap().consistent);
    }
}
