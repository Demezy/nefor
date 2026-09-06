//! Successful project artifacts only. The ordinary compiler APIs remain cold.
use crate::error::MagError;
use crate::observation::{compile_file_observed, ObservationSet};
use crate::profile::CompileProfiler;
use crate::FileCompileRequest;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

const FORMAT: &str = "mag-project-cache-v1";
const OUTPUT: &str = "mag-json-line-v1";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CachePolicy {
    Use,
    Bypass,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CompilerBuildId(String);
impl CompilerBuildId {
    /// Hash the executable on every cache-use operation; pathname/version/mtime
    /// are not compiler identities. Failure disables caching, not compilation.
    pub fn current() -> io::Result<Self> {
        let mut file = File::open(std::env::current_exe()?)?;
        let mut hash = Sha256::new();
        let mut buffer = [0; 65536];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        Ok(Self(format!("{:x}", hash.finalize())))
    }

    /// Explicit identity seam for isolated tests/benchmark endpoints, not CLI configuration.
    pub fn from_executable_bytes(bytes: &[u8]) -> Self {
        Self(digest(bytes))
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheStatus {
    Hit,
    Miss,
    Bypass,
    Unavailable,
}

#[derive(Debug, Serialize)]
pub struct CacheProfile {
    pub status: CacheStatus,
    /// Executable hashing, request identity, candidate integrity and input validation.
    pub lookup_duration_ns: u64,
    pub publication_duration_ns: u64,
}

#[derive(Debug)]
pub struct BuildOutput {
    pub bytes: Vec<u8>,
    pub cache: CacheProfile,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    schema: String,
    compiler: CompilerBuildId,
    output: String,
    root: String,
    entry: String,
    roots: Vec<String>,
    inputs: serde_json::Value,
    config_version: u32,
    evaluation_steps: u64,
    call_depth: u16,
    expression_depth: u16,
    memoized_calls: usize,
}
impl Identity {
    fn new(
        request: &FileCompileRequest<'_>,
        version: u32,
        compiler: CompilerBuildId,
    ) -> Option<Self> {
        if !request.source_dir.is_absolute()
            || request.module_roots.iter().any(|p| !p.is_absolute())
        {
            return None;
        }
        let limits = request.options.limits;
        Some(Self {
            schema: FORMAT.into(),
            compiler,
            output: OUTPUT.into(),
            root: request.source_dir.to_str()?.into(),
            entry: request.entry.into(),
            roots: request
                .module_roots
                .iter()
                .map(|p| p.to_str().map(str::to_owned))
                .collect::<Option<_>>()?,
            inputs: request.inputs.clone(),
            config_version: version,
            evaluation_steps: limits.evaluation_steps,
            call_depth: limits.call_depth,
            expression_depth: limits.expression_depth,
            memoized_calls: limits.memoized_calls,
        })
    }
    fn bucket(&self, cache_dir: &Path) -> Option<PathBuf> {
        Some(
            cache_dir
                .join("v1")
                .join(&self.compiler.0)
                .join(digest(&serde_json::to_vec(self).ok()?)),
        )
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Provenance {
    format: String,
    request: Identity,
    output: String,
    observations: ObservationSet,
    artifact_length: u64,
    artifact_sha256: String,
}

pub fn serialize_artifact(artifact: &serde_json::Value) -> Result<Vec<u8>, MagError> {
    let mut bytes = serde_json::to_vec(artifact)
        .map_err(|e| MagError::Eval(format!("cannot serialize artifact: {e}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn build(
    request: FileCompileRequest<'_>,
    config_version: u32,
    policy: CachePolicy,
    profiler: Option<&CompileProfiler>,
) -> Result<BuildOutput, MagError> {
    build_with_identity(
        request,
        config_version,
        policy,
        profiler,
        CompilerBuildId::current,
    )
}

/// The identity supplier is never invoked for bypass. It keeps benchmark
/// identities explicit without an environment override in production.
pub fn build_with_identity(
    request: FileCompileRequest<'_>,
    config_version: u32,
    policy: CachePolicy,
    profiler: Option<&CompileProfiler>,
    identity: impl FnOnce() -> io::Result<CompilerBuildId>,
) -> Result<BuildOutput, MagError> {
    let cache_dir = request.source_dir.join(".mag/cache");
    build_in_with_identity(
        request,
        config_version,
        &cache_dir,
        policy,
        profiler,
        identity,
    )
}

/// Build using caller-owned writable storage, without changing request identity.
/// `cache_dir` is the cache root (containing `v1`), not a project root.
/// Unavailable storage falls back to cold compilation as with root-local builds.
pub fn build_in(
    request: FileCompileRequest<'_>,
    config_version: u32,
    cache_dir: &Path,
    policy: CachePolicy,
    profiler: Option<&CompileProfiler>,
) -> Result<BuildOutput, MagError> {
    build_in_with_identity(
        request,
        config_version,
        cache_dir,
        policy,
        profiler,
        CompilerBuildId::current,
    )
}

/// Explicit executable identity seam for embedding tests and benchmarks.
pub fn build_in_with_identity(
    request: FileCompileRequest<'_>,
    config_version: u32,
    cache_dir: &Path,
    policy: CachePolicy,
    profiler: Option<&CompileProfiler>,
    identity: impl FnOnce() -> io::Result<CompilerBuildId>,
) -> Result<BuildOutput, MagError> {
    let started = Instant::now();
    let identity = if policy == CachePolicy::Use {
        identity()
            .ok()
            .and_then(|id| Identity::new(&request, config_version, id))
    } else {
        None
    };
    let bucket = identity.as_ref().and_then(|id| id.bucket(cache_dir));
    if let (Some(identity), Some(bucket)) = (&identity, &bucket) {
        if let Some(bytes) = lookup(bucket, identity) {
            return Ok(BuildOutput {
                bytes,
                cache: CacheProfile {
                    status: CacheStatus::Hit,
                    lookup_duration_ns: nanos(started),
                    publication_duration_ns: 0,
                },
            });
        }
    }
    let mut cache = CacheProfile {
        status: if policy == CachePolicy::Bypass {
            CacheStatus::Bypass
        } else if bucket.is_some() {
            CacheStatus::Miss
        } else {
            CacheStatus::Unavailable
        },
        lookup_duration_ns: nanos(started),
        publication_duration_ns: 0,
    };
    let bytes = if let (Some(identity), Some(bucket)) = (identity, bucket) {
        let compiled = compile_file_observed(request, profiler)?;
        let bytes = serialize_artifact(&compiled.artifact)?;
        let started = Instant::now();
        let provenance = Provenance {
            format: FORMAT.into(),
            request: identity,
            output: OUTPUT.into(),
            observations: compiled.observations,
            artifact_length: bytes.len() as u64,
            artifact_sha256: digest(&bytes),
        };
        // Storage is an optional acceleration. Every failure preserves cold output.
        let _ = publish(&bucket, &provenance, &bytes);
        cache.publication_duration_ns = nanos(started);
        bytes
    } else {
        let session = crate::CompilerSession::new();
        let artifact = match profiler {
            Some(profiler) => session.compile_file_with_profiler(request, profiler),
            None => session.compile_file(request),
        }?;
        serialize_artifact(&artifact)?
    };
    Ok(BuildOutput { bytes, cache })
}

fn nanos(start: Instant) -> u64 {
    start.elapsed().as_nanos().min(u64::MAX as u128) as u64
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn record_name(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn lookup(bucket: &Path, identity: &Identity) -> Option<Vec<u8>> {
    let mut candidates = fs::read_dir(bucket)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_str().is_some_and(record_name))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    candidates.sort();
    candidates
        .into_iter()
        .find_map(|path| load_record(&path, identity))
}
fn load_record(path: &Path, identity: &Identity) -> Option<Vec<u8>> {
    let sidecar = fs::read(path.join("provenance.json")).ok()?;
    if path.file_name()?.to_str()? != digest(&sidecar) {
        return None;
    }
    let provenance: Provenance = serde_json::from_slice(&sidecar).ok()?;
    if provenance.format != FORMAT || provenance.output != OUTPUT || &provenance.request != identity
    {
        return None;
    }
    let artifact = fs::read(path.join("artifact.json")).ok()?;
    if artifact.len() as u64 != provenance.artifact_length
        || digest(&artifact) != provenance.artifact_sha256
        || !artifact.ends_with(b"\n")
        || artifact[..artifact.len() - 1].contains(&b'\n')
        || !valid_json(&artifact)
        || !provenance.observations.validate()
    {
        return None;
    }
    Some(artifact)
}

fn valid_json(bytes: &[u8]) -> bool {
    // IgnoredAny validates JSON syntax with serde_json's iterative skip parser,
    // without constructing a Value or imposing its deserialization depth limit.
    // Validate UTF-8 separately because skipped strings need not be decoded.
    std::str::from_utf8(bytes)
        .ok()
        .is_some_and(|text| serde_json::from_str::<serde::de::IgnoredAny>(text).is_ok())
}

struct TemporaryDirectory(PathBuf);
impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}
fn publish(bucket: &Path, provenance: &Provenance, bytes: &[u8]) -> io::Result<()> {
    let sidecar = serde_json::to_vec(provenance)?;
    let destination = bucket.join(digest(&sidecar));
    fs::create_dir_all(bucket)?;
    let temporary = loop {
        let path = bucket.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::create_dir(&path) {
            Ok(()) => break TemporaryDirectory(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    write_synced(&temporary.0.join("artifact.json"), bytes)?;
    write_synced(&temporary.0.join("provenance.json"), &sidecar)?;
    File::open(&temporary.0)?.sync_all()?;
    if !provenance.observations.validate() {
        return Ok(());
    }
    // A competing identical publication is harmless. Never replace a record.
    match fs::rename(&temporary.0, &destination) {
        Ok(()) => File::open(bucket)?.sync_all(),
        Err(_) if destination.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
#[path = "project_cache_tests.rs"]
mod tests;
