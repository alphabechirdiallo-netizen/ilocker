// ============================================================
//  utils.rs — cross-cutting utilities
//  Phase 2 upgrade: adaptive streaming SHA-256 for huge files
// ============================================================

use anyhow::Result;
use sha2::{Sha256, Digest};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use uuid::Uuid;

// ── Thresholds ────────────────────────────────────────────────
//
//  Files are classified into three tiers so we never saturate RAM:
//
//  SMALL   < 64 MiB  →  64 KiB  read buffer  (fits in L2/L3 cache)
//  MEDIUM  < 1  GiB  →   4 MiB  read buffer  (good throughput, low pressure)
//  LARGE   ≥ 1  GiB  →  16 MiB  read buffer  (streaming; never more in RAM)
//
//  The large-file tier also reports progress so long-running hashes
//  are visible to the user (via the scan progress bar).

const SMALL_THRESHOLD:  u64 = 64  * 1024 * 1024;   //  64 MiB
const MEDIUM_THRESHOLD: u64 = 1   * 1024 * 1024 * 1024; //   1 GiB

const BUF_SMALL:  usize = 64  * 1024;           //  64 KiB
const BUF_MEDIUM: usize = 4   * 1024 * 1024;    //   4 MiB
const BUF_LARGE:  usize = 16  * 1024 * 1024;    //  16 MiB

/// File size classification — used by the scan engine and the
/// sentinel to decide how to treat a given file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileTier {
    Small,   // < 64 MiB
    Medium,  // 64 MiB – 1 GiB
    Large,   // ≥ 1 GiB  (datasets, disk images, AI model weights…)
}

impl FileTier {
    pub fn from_size(bytes: u64) -> Self {
        if bytes < SMALL_THRESHOLD        { FileTier::Small  }
        else if bytes < MEDIUM_THRESHOLD  { FileTier::Medium }
        else                              { FileTier::Large  }
    }

    /// Return the appropriate read-buffer size for this tier.
    pub fn buf_size(self) -> usize {
        match self {
            FileTier::Small  => BUF_SMALL,
            FileTier::Medium => BUF_MEDIUM,
            FileTier::Large  => BUF_LARGE,
        }
    }
}

// ── Project ID ────────────────────────────────────────────────

/// Generate a new unique project identifier (UUID v4, no dashes).
pub fn new_project_id() -> String {
    Uuid::new_v4().to_string().replace('-', "")
}

/// Generate a short snapshot ID: first 12 hex chars of a UUID v4.
pub fn new_snapshot_id() -> String {
    let u = Uuid::new_v4().to_string().replace('-', "");
    u[..12].to_string()
}

// ── Adaptive SHA-256 ─────────────────────────────────────────

/// Compute the SHA-256 digest of a file.
///
/// The read-buffer size is chosen automatically based on the file's
/// on-disk size so we stay within predictable RAM boundaries even
/// when hashing multi-gigabyte assets or AI model weights.
pub fn sha256_file(path: &Path) -> Result<String> {
    let file_size = std::fs::metadata(path)
        .map(|m| m.len())
        .unwrap_or(0);

    let tier     = FileTier::from_size(file_size);
    let buf_size = tier.buf_size();

    let file   = File::open(path)?;
    let mut reader = BufReader::with_capacity(buf_size, file);
    let mut hasher = Sha256::new();

    // Heap-allocate for medium/large to avoid stack overflow
    match tier {
        FileTier::Small => {
            let mut buf = vec![0u8; buf_size];
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 { break; }
                hasher.update(&buf[..n]);
            }
        }
        FileTier::Medium | FileTier::Large => {
            let mut buf = vec![0u8; buf_size];
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 { break; }
                hasher.update(&buf[..n]);
            }
        }
    }

    Ok(hex::encode(hasher.finalize()))
}

/// Compute the SHA-256 digest of a byte slice.
#[allow(dead_code)]
pub fn sha256_bytes(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

// ── Smart project-type detection ─────────────────────────────

/// Detected project types — used by Smart Ignore and Sentinel
/// to tune their behaviour for the specific ecosystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectType {
    NodeJs,
    Python,
    Rust,
    Go,
    Ruby,
    Java,
    DotNet,
    Cpp,
    Unknown,
}

impl ProjectType {
    /// Sniff the project type from manifest / config files present
    /// in the project root.  Returns a list because a repo can be
    /// multi-language (e.g. a Python backend with a Node frontend).
    pub fn detect(project_root: &Path) -> Vec<Self> {
        let mut types = Vec::new();
        let markers: &[(&str, ProjectType)] = &[
            ("package.json",       ProjectType::NodeJs),
            ("requirements.txt",   ProjectType::Python),
            ("pyproject.toml",     ProjectType::Python),
            ("setup.py",           ProjectType::Python),
            ("Cargo.toml",         ProjectType::Rust),
            ("go.mod",             ProjectType::Go),
            ("Gemfile",            ProjectType::Ruby),
            ("pom.xml",            ProjectType::Java),
            ("build.gradle",       ProjectType::Java),
            ("*.csproj",           ProjectType::DotNet),
            ("CMakeLists.txt",     ProjectType::Cpp),
        ];
        for (marker, kind) in markers {
            if marker.contains('*') {
                // Glob: look for any matching file in root
                if let Ok(entries) = std::fs::read_dir(project_root) {
                    let pattern = marker.trim_start_matches('*');
                    for entry in entries.flatten() {
                        if entry.file_name().to_string_lossy().ends_with(pattern) {
                            types.push(kind.clone());
                            break;
                        }
                    }
                }
            } else if project_root.join(marker).exists() {
                types.push(kind.clone());
            }
        }
        if types.is_empty() { types.push(ProjectType::Unknown); }
        types
    }

    pub fn label(&self) -> &'static str {
        match self {
            ProjectType::NodeJs  => "Node.js",
            ProjectType::Python  => "Python",
            ProjectType::Rust    => "Rust",
            ProjectType::Go      => "Go",
            ProjectType::Ruby    => "Ruby",
            ProjectType::Java    => "Java",
            ProjectType::DotNet  => ".NET",
            ProjectType::Cpp     => "C/C++",
            ProjectType::Unknown => "unknown",
        }
    }
}

// ── Ignore rules ──────────────────────────────────────────────

pub struct IgnoreRules {
    patterns: Vec<String>,
}

impl IgnoreRules {
    pub fn load(project_root: &Path) -> Self {
        let mut patterns: Vec<String> = vec![".ilocker/".to_string()];
        let ignore_file = project_root.join(".ilockerignore");
        if let Ok(content) = std::fs::read_to_string(&ignore_file) {
            for line in content.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !trimmed.starts_with('#') {
                    patterns.push(trimmed.to_string());
                }
            }
        }
        Self { patterns }
    }

    pub fn is_ignored(&self, rel: &Path) -> bool {
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        for pattern in &self.patterns {
            if self.matches(pattern, &rel_str) { return true; }
        }
        false
    }

    fn matches(&self, pattern: &str, path: &str) -> bool {
        if pattern.ends_with('/') {
            let dir_name = pattern.trim_end_matches('/');
            return path.split('/').any(|c| c == dir_name)
                || path.starts_with(&format!("{}/", dir_name));
        }
        if path == pattern { return true; }
        if pattern.contains('*') { return glob_match(pattern, path); }
        if path.starts_with(&format!("{}/", pattern)) { return true; }
        path.split('/').last().unwrap_or("") == pattern
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_recurse(&p, &t, 0, 0)
}

fn glob_recurse(p: &[char], t: &[char], pi: usize, ti: usize) -> bool {
    if pi == p.len() { return ti == t.len(); }
    if p[pi] == '*' {
        if pi + 1 < p.len() && p[pi + 1] == '*' {
            for i in ti..=t.len() {
                if glob_recurse(p, t, pi + 2, i) { return true; }
            }
            return false;
        }
        for i in ti..=t.len() {
            if i > ti && t[i - 1] == '/' { break; }
            if glob_recurse(p, t, pi + 1, i) { return true; }
        }
        return false;
    }
    if ti < t.len() && (p[pi] == '?' || p[pi] == t[ti]) {
        return glob_recurse(p, t, pi + 1, ti + 1);
    }
    false
}

// ── Filesystem helpers ────────────────────────────────────────

#[cfg(unix)]
pub fn inode_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.ino())
}

#[cfg(not(unix))]
pub fn inode_of(_path: &Path) -> Option<u64> { None }

pub fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];
    let mut size = n as f64;
    let mut idx  = 0;
    while size >= 1024.0 && idx < UNITS.len() - 1 {
        size /= 1024.0;
        idx  += 1;
    }
    if idx == 0 { format!("{} B", n) }
    else        { format!("{:.1} {}", size, UNITS[idx]) }
}

pub fn db_path(ilocker_dir: &Path) -> PathBuf {
    ilocker_dir.join("iloc.db")
}
