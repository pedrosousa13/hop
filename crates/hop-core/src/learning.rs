//! Frecency learning: records that a query led to launching an item, and
//! later reports a boost for that pairing, so frequently-chosen results rise
//! for the queries that reach them.
//!
//! Ported (salvage, not rewritten — the audit marked this module worth
//! keeping) from the abandoned `feat/cross-linux-hopd-v1` branch's
//! `hopd::learning::LearningStore`. The differences from that source are
//! deliberate interface decisions, not fixes:
//!
//! - Renamed `LearningStore` to [`Learning`], with fully private state (the
//!   salvage exposed `version`, `selections` and `global_frequency` as
//!   `pub`).
//! - Dropped the stored `path` field: [`Learning::load`] and [`Learning::save`]
//!   both take a `&Path` explicitly instead, so nothing here remembers where
//!   it lives on disk.
//! - [`Learning::save`] takes `&self` (not `&mut self`) and returns
//!   `std::io::Result<()>` instead of swallowing every error. Since it can't
//!   mutate through `&self`, it computes the purged, canonicalized payload
//!   into a local value rather than calling `purge_expired` on `self`.
//! - [`Learning::record_launch`] and [`Learning::boost_for`] are new,
//!   `ItemId`-keyed public entry points. `boost_for` sums the salvage's
//!   `query_boost` and `frequency_boost`, clamped to
//!   `0.0..=LEARNING_BOOST_CAP` — that clamp is the only place the cap
//!   applies; `query_boost`/`frequency_boost` keep their original `i32`
//!   scale and internal caps (150, 60) unmodified.
//! - `reset` no longer self-persists (it has no path to persist to); it only
//!   clears in-memory state now.
//!
//! Nothing outside `load` and `save` touches the filesystem.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use hop_protocol::ItemId;

/// The maximum boost [`Learning::boost_for`] can ever return. Must sit
/// strictly below the alias boost constant (180.0, arriving with the
/// aliases module in M1.6) — aliases are an explicit user instruction and
/// must always beat learned behavior. Exported so the aliases module and
/// its tests can assert that relationship directly.
pub const LEARNING_BOOST_CAP: f32 = 85.0;

// --- Constants (unchanged from the salvage) ---

const MAX_QUERIES: usize = 500;
const MAX_ITEMS_PER_QUERY: usize = 20;
const MAX_GLOBAL_ENTRIES: usize = 1000;

const QUERY_BOOST_PER_COUNT: i32 = 15;
const QUERY_BOOST_CAP: i32 = 150;

const FREQ_BOOST_PER_COUNT: i32 = 3;
const FREQ_BOOST_CAP: i32 = 60;

/// 30 days in milliseconds — half-life for decay.
const DECAY_HALF_MS: u64 = 30 * 24 * 60 * 60 * 1000;
/// 90 days in milliseconds — quarter-life for decay.
const DECAY_QUARTER_MS: u64 = 90 * 24 * 60 * 60 * 1000;
/// Hard retention cutoff for persisted learning data.
const PERSIST_RETENTION_MS: u64 = 90 * 24 * 60 * 60 * 1000;

// --- Data types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LearningEntry {
    count: u32,
    last_ms: u64,
}

/// Frecency state: per-query selection history plus a global launch
/// frequency table, both decayed by recency and capped in size.
///
/// State is private. The contract this slice was specified against is
/// [`Learning::load`], [`Learning::save`], [`Learning::record_launch`] and
/// [`Learning::boost_for`]; [`Learning::reset`], [`Learning::recent_launches`],
/// [`Learning::frequent_launches`] and [`Learning::is_empty`] are also
/// public, carried over from the salvage as-is for later milestones (surfacing
/// learning insights to the user is explicitly out of scope here).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Learning {
    version: u32,
    #[serde(default, skip_serializing)]
    selections: HashMap<String, HashMap<String, LearningEntry>>,
    global_frequency: HashMap<String, LearningEntry>,
}

/// The on-disk shape: per-query selections are intentionally left out, so
/// raw query text never lands on disk.
#[derive(Debug, Serialize, Deserialize)]
struct PersistedLearningStore {
    version: u32,
    global_frequency: HashMap<String, LearningEntry>,
}

// --- Helper functions (unchanged from the salvage) ---

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Apply recency decay to a raw boost value.
/// Returns full value if within half-life, halved if within quarter-life, quartered beyond.
fn apply_decay(raw: i32, last_ms: u64, now: u64) -> i32 {
    if now <= last_ms {
        return raw;
    }
    let age = now - last_ms;
    if age <= DECAY_HALF_MS {
        raw
    } else if age <= DECAY_QUARTER_MS {
        raw / 2
    } else {
        raw / 4
    }
}

/// Evict least-recently-used entries from an inner map (result_id -> LearningEntry)
/// until the map size is at most `max`.
fn evict_lru_map(map: &mut HashMap<String, LearningEntry>, max: usize) {
    while map.len() > max {
        if let Some(oldest_key) = map
            .iter()
            .min_by_key(|(_, entry)| entry.last_ms)
            .map(|(key, _)| key.clone())
        {
            map.remove(&oldest_key);
        } else {
            break;
        }
    }
}

/// Evict least-recently-used outer keys from the selections map
/// until the map size is at most `max`. The "age" of an outer key is
/// the maximum last_ms across its inner entries.
fn evict_lru_outer(map: &mut HashMap<String, HashMap<String, LearningEntry>>, max: usize) {
    while map.len() > max {
        if let Some(oldest_key) = map
            .iter()
            .map(|(key, inner)| {
                let max_ms = inner.values().map(|e| e.last_ms).max().unwrap_or(0);
                (key.clone(), max_ms)
            })
            .min_by_key(|(_, ms)| *ms)
            .map(|(key, _)| key)
        {
            map.remove(&oldest_key);
        } else {
            break;
        }
    }
}

fn canonicalize_result_id(result_id: &str) -> String {
    if let Some(utility_tail) = result_id.strip_prefix("utility:") {
        let utility_kind = utility_tail.split(':').next().unwrap_or_default();
        if !utility_kind.is_empty() {
            return format!("utility:{utility_kind}");
        }
    }
    if let Some(web_tail) = result_id.strip_prefix("web-search:") {
        let service = web_tail.split(':').next().unwrap_or_default();
        if !service.is_empty() {
            return format!("web-search:{service}");
        }
    }
    result_id.to_string()
}

fn canonicalized_global_frequency(
    input: &HashMap<String, LearningEntry>,
) -> HashMap<String, LearningEntry> {
    let mut out: HashMap<String, LearningEntry> = HashMap::new();
    for (id, entry) in input {
        let key = canonicalize_result_id(id);
        let aggregate = out.entry(key).or_insert(LearningEntry {
            count: 0,
            last_ms: 0,
        });
        aggregate.count = aggregate.count.saturating_add(entry.count);
        aggregate.last_ms = aggregate.last_ms.max(entry.last_ms);
    }
    out
}

/// Entries in `global_frequency` older than the retention cutoff, purged.
/// Split out of `save` so it can be applied to a local clone rather than
/// mutating `self` (see the module docs — `save` takes `&self`).
fn purge_retention(
    global_frequency: &HashMap<String, LearningEntry>,
) -> HashMap<String, LearningEntry> {
    let cutoff = now_ms().saturating_sub(PERSIST_RETENTION_MS);
    let mut purged = global_frequency.clone();
    purged.retain(|_, entry| entry.last_ms >= cutoff);
    purged
}

// --- Learning implementation ---

impl Learning {
    /// An empty state: version 1, no selections, no global frequency.
    fn empty() -> Self {
        Self {
            version: 1,
            selections: HashMap::new(),
            global_frequency: HashMap::new(),
        }
    }

    /// Load from disk, falling back to an empty state on any error — a
    /// missing file, unparseable bytes, or valid JSON of the wrong shape all
    /// land here. Never panics.
    pub fn load(path: &Path) -> Learning {
        if let Ok(data) = std::fs::read_to_string(path) {
            if let Ok(persisted) = serde_json::from_str::<PersistedLearningStore>(&data) {
                let mut store = Self::empty();
                store.version = persisted.version;
                store.global_frequency = persisted.global_frequency;
                store.purge_expired();
                return store;
            }
            if let Ok(mut store) = serde_json::from_str::<Learning>(&data) {
                store.selections.clear();
                store.purge_expired();
                return store;
            }
        }
        Self::empty()
    }

    /// Persist to disk via a temp file + atomic rename, mode 0600. Creates
    /// the parent directory if it doesn't exist yet, at mode 0700 on unix —
    /// but a parent that already exists is left exactly as found, whatever
    /// its mode. `persist_atomically`'s `DirBuilder` block says why that
    /// asymmetry is load-bearing.
    ///
    /// Per-query selections are never written — only the canonicalized,
    /// retention-purged global frequency table is.
    ///
    /// A store that cannot be serialized returns `Err` having created
    /// nothing at all — no file, no directory — so whatever is already on
    /// disk survives intact. `serialize_and_persist` is where that ordering
    /// lives.
    ///
    /// This is the only other entry point (besides `load`) that touches the
    /// filesystem.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let purged_global = purge_retention(&self.global_frequency);
        serialize_and_persist(
            path,
            &PersistedLearningStore {
                version: self.version,
                global_frequency: canonicalized_global_frequency(&purged_global),
            },
        )
    }

    /// Record a launch: the user reached `item_id` while typing `query`.
    pub fn record_launch(&mut self, query: &str, item_id: &ItemId) {
        self.record(query, &item_id.0);
    }

    /// Record a selection: the user chose `result_id` while typing `query`.
    fn record(&mut self, query: &str, result_id: &str) {
        self.purge_expired();
        let ts = now_ms();
        let normalized = query.trim().to_lowercase();

        // Update per-query selections
        let inner = self.selections.entry(normalized).or_default();
        let entry = inner.entry(result_id.to_string()).or_insert(LearningEntry {
            count: 0,
            last_ms: 0,
        });
        entry.count = entry.count.saturating_add(1);
        entry.last_ms = ts;

        // Evict inner map if too large
        evict_lru_map(inner, MAX_ITEMS_PER_QUERY);

        // Evict outer map if too large
        evict_lru_outer(&mut self.selections, MAX_QUERIES);

        // Update global frequency
        let global = self
            .global_frequency
            .entry(result_id.to_string())
            .or_insert(LearningEntry {
                count: 0,
                last_ms: 0,
            });
        global.count = global.count.saturating_add(1);
        global.last_ms = ts;

        // Evict global map if too large
        evict_lru_map(&mut self.global_frequency, MAX_GLOBAL_ENTRIES);
    }

    /// Clear all learned data. Unlike the salvage's `reset`, this does not
    /// persist — `Learning` no longer owns a path to persist to. Callers
    /// that want the clear to survive a restart must call `save` themselves.
    pub fn reset(&mut self) {
        self.selections.clear();
        self.global_frequency.clear();
    }

    /// Compute a query-specific boost for `result_id`.
    ///
    /// Prefix matching works both ways:
    /// - A shorter stored key that is a prefix of `query` contributes.
    /// - A longer stored key that starts with `query` also contributes.
    ///
    /// The boost is count * QUERY_BOOST_PER_COUNT, with recency decay, capped at QUERY_BOOST_CAP.
    fn query_boost(&self, query: &str, result_id: &str) -> i32 {
        let normalized = query.trim().to_lowercase();
        if normalized.is_empty() {
            return 0;
        }
        let now = now_ms();
        let mut total: i32 = 0;

        for (stored_query, inner) in &self.selections {
            // Prefix match: either stored_query is a prefix of the current query
            // or the current query is a prefix of the stored_query.
            let is_prefix_match = normalized.starts_with(stored_query.as_str())
                || stored_query.starts_with(&normalized);
            if !is_prefix_match {
                continue;
            }
            if let Some(entry) = inner.get(result_id) {
                let raw = (entry.count as i32).saturating_mul(QUERY_BOOST_PER_COUNT);
                total = total.saturating_add(apply_decay(raw, entry.last_ms, now));
            }
        }

        total.min(QUERY_BOOST_CAP)
    }

    /// Compute a global frequency boost for `result_id`, with recency decay, capped at FREQ_BOOST_CAP.
    fn frequency_boost(&self, result_id: &str) -> i32 {
        let now = now_ms();
        if let Some(entry) = self.global_frequency.get(result_id) {
            let raw = (entry.count as i32).saturating_mul(FREQ_BOOST_PER_COUNT);
            apply_decay(raw, entry.last_ms, now).min(FREQ_BOOST_CAP)
        } else {
            0
        }
    }

    /// The learned boost for this query/item pairing: the sum of
    /// `query_boost` and `frequency_boost`, clamped to
    /// `0.0..=LEARNING_BOOST_CAP`. This is the value the ranker consumes.
    pub fn boost_for(&self, query: &str, item_id: &ItemId) -> f32 {
        let total = self.query_boost(query, &item_id.0) + self.frequency_boost(&item_id.0);
        (total as f32).clamp(0.0, LEARNING_BOOST_CAP)
    }

    /// Return the most recently launched result IDs, sorted by last_ms descending.
    pub fn recent_launches(&self, limit: usize) -> Vec<(String, u64)> {
        let mut entries: Vec<(String, u64)> = self
            .global_frequency
            .iter()
            .map(|(id, entry)| (id.clone(), entry.last_ms))
            .collect();
        entries.sort_by_key(|(_, last_ms)| std::cmp::Reverse(*last_ms));
        entries.truncate(limit);
        entries
    }

    /// Return the most frequently launched result IDs, sorted by count descending,
    /// excluding the given IDs.
    pub fn frequent_launches(&self, limit: usize, exclude: &[String]) -> Vec<(String, u32)> {
        let mut entries: Vec<(String, u32)> = self
            .global_frequency
            .iter()
            .filter(|(id, _)| !exclude.contains(id))
            .map(|(id, entry)| (id.clone(), entry.count))
            .collect();
        entries.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        entries.truncate(limit);
        entries
    }

    /// Returns true if there are no selections and no global frequency entries.
    pub fn is_empty(&self) -> bool {
        self.selections.is_empty() && self.global_frequency.is_empty()
    }

    fn purge_expired(&mut self) {
        let cutoff = now_ms().saturating_sub(PERSIST_RETENTION_MS);
        self.selections.retain(|_, inner| {
            inner.retain(|_, entry| entry.last_ms >= cutoff);
            !inner.is_empty()
        });
        self.global_frequency
            .retain(|_, entry| entry.last_ms >= cutoff);
    }
}

/// Serialize `value`, then persist it — and only in that order. A value
/// that fails to serialize returns `Err` before `persist_atomically` runs,
/// so nothing on disk is touched: not the store, not even its parent
/// directory. `persist_atomically` takes a `&str`, so there is no way to
/// reach the filesystem without a payload that already serialized.
///
/// Generic over `T` rather than taking `&PersistedLearningStore` so this
/// stays the seam the failure is tested through. The persisted shape is
/// `String` keys over `u32`/`u64` today, which `to_string_pretty` cannot
/// fail on, so no test could provoke the failure through
/// [`Learning::save`]. Handing this a value that genuinely fails to
/// serialize exercises the real ordering, and keeps the guard honest for
/// the day the persisted shape grows a field that can fail.
fn serialize_and_persist<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    // `InvalidData` because that is what this is from the filesystem's point
    // of view: data that cannot be represented on disk. The
    // `unwrap_or_default()` that used to stand here turned the same
    // condition into an empty payload, wrote it over a good store, and
    // reported success.
    let payload = serde_json::to_string_pretty(value)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    persist_atomically(path, &payload)
}

/// The filesystem half of [`Learning::save`]: create the parent directory
/// if it is missing, write `payload` to a temp file beside the destination,
/// rename it into place, and narrow the result to 0600 on unix.
///
/// Layered over [`write_and_rename`], which is only the temp-file half of
/// the sequence; this is the function that owns the surrounding directory
/// and permission steps, and the temp file's cleanup on failure.
fn persist_atomically(path: &Path, payload: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        // A directory this code creates is narrowed to 0700; one that was
        // already there is left exactly as found, whatever its mode. That
        // asymmetry is deliberate and load-bearing: `path` is derived from
        // `XDG_STATE_HOME`, which the user controls and which this module
        // never sees, so we can only reason about a directory we created
        // ourselves. A user who exports `XDG_STATE_HOME=$HOME` must not
        // find their home directory silently narrowed to 0700 on first
        // launch, and `Learning::save` is the only persistence entry point,
        // so there would be no way to opt out.
        //
        // `DirBuilder` rather than `create_dir_all` + `set_permissions`,
        // for three reasons that all come down to only ever touching a
        // directory we made:
        //
        // - The mode is an argument to `mkdir(2)`, so the directory is
        //   born at 0700. There is no window at a wider mode to race,
        //   unlike a create-then-chmod pair. `mkdir` masks the mode with
        //   the umask (`mode & ~umask`), which can only clear bits — and
        //   0700 has no group or other bits to begin with — so no umask
        //   can widen this, only narrow it to something useless-but-safe.
        // - `recursive(true)` makes an existing path a no-op rather than
        //   an error: std's `mkdir` returns `EEXIST`, std confirms the
        //   path is a directory, and stops. Both syscalls are read-only
        //   with respect to the mode, so a pre-existing parent keeps
        //   whatever it had by construction — not by a check we could
        //   forget to write.
        // - That also disposes of symlinks. `chmod(2)` follows them and
        //   would have narrowed a symlinked parent's *target*; `mkdir(2)`
        //   on a symlink to a directory just returns `EEXIST`.
        //
        // Recursive creation applies 0700 to every component it creates,
        // not only the leaf — if `~/.local/state` is missing too, it is
        // created at 0700 as well. That is correct under the same rule:
        // we created it, so it is ours to narrow. Anything that already
        // existed is skipped, so the reach stops at the first component
        // we did not make.
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder.create(parent)?;
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path has no valid file name")
        })?;
    let temp_name = format!(
        ".{}.tmp-{}-{}",
        file_name,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let temp_path = parent.join(temp_name);

    let result = write_and_rename(&temp_path, path, payload.as_bytes());
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result?;

    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Write `payload` to `temp_path` (create/truncate, mode 0600 on unix),
/// fsync it, then rename it onto `dest`. Split out of [`persist_atomically`]
/// purely to keep the `?`-propagation linear; the caller is responsible for
/// cleaning up `temp_path` on error.
fn write_and_rename(temp_path: &Path, dest: &Path, payload: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(temp_path)?;
    file.write_all(payload)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(temp_path, dest)?;
    Ok(())
}

// --- Tests ---

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    // --- Ported from the salvage, behavior unmodified. ---

    #[test]
    fn record_and_query_selection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning_test_nonexistent.json");
        let mut store = Learning::load(&path);

        store.record("fire", "app:firefox");
        store.record("fire", "app:firefox");
        store.record("fire", "app:firewall");
        store.record("code", "app:vscode");

        // firefox was selected twice for "fire"
        let inner = store.selections.get("fire").unwrap();
        assert_eq!(inner.get("app:firefox").unwrap().count, 2);
        assert_eq!(inner.get("app:firewall").unwrap().count, 1);

        // global frequency
        assert_eq!(store.global_frequency.get("app:firefox").unwrap().count, 2);
        assert_eq!(store.global_frequency.get("app:firewall").unwrap().count, 1);
        assert_eq!(store.global_frequency.get("app:vscode").unwrap().count, 1);

        // query_boost should be positive for a matching query/result pair
        let boost = store.query_boost("fire", "app:firefox");
        assert!(boost > 0, "expected positive boost, got {boost}");

        // frequency_boost should be positive
        let freq = store.frequency_boost("app:firefox");
        assert!(freq > 0, "expected positive freq boost, got {freq}");
    }

    #[test]
    fn save_and_load_round_trip_without_persisting_query_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");

        let mut store = Learning::load(&path);
        store.record("fire", "app:firefox");
        store.record("fire", "app:firefox");
        store.save(&path).unwrap();

        let saved = std::fs::read_to_string(&path).expect("saved learning file");
        assert!(
            !saved.contains("\"fire\""),
            "raw query keys should not be persisted"
        );

        let loaded = Learning::load(&path);
        assert_eq!(loaded.global_frequency.get("app:firefox").unwrap().count, 2);
        assert!(
            loaded.selections.is_empty(),
            "query selections should remain in-memory only after reload"
        );
    }

    #[test]
    fn canonicalizes_dynamic_result_ids_for_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");

        let mut store = Learning::load(&path);
        store.record(
            "rust docs",
            "web-search:duckduckgo:https%3A%2F%2Fduckduckgo.com%2F%3Fq%3Drust%2Bdocs",
        );
        store.record("2+2", "utility:calculator:2+2");
        store.save(&path).unwrap();

        let loaded = Learning::load(&path);
        assert!(
            loaded
                .global_frequency
                .contains_key("web-search:duckduckgo"),
            "web-search ids should strip query payloads before persistence"
        );
        assert!(
            loaded.global_frequency.contains_key("utility:calculator"),
            "utility ids should strip dynamic suffixes before persistence"
        );
    }

    #[test]
    fn empty_store_returns_no_boosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning_test_empty_nonexistent.json");
        let store = Learning::load(&path);

        assert!(store.is_empty());
        assert_eq!(store.query_boost("anything", "app:foo"), 0);
        assert_eq!(store.frequency_boost("app:foo"), 0);
        assert!(store.recent_launches(10).is_empty());
        assert!(store.frequent_launches(10, &[]).is_empty());
    }

    #[test]
    fn lru_eviction_respects_max_queries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut store = Learning::load(&path);

        for i in 0..510 {
            store.record(&format!("query{i}"), "some.desktop");
        }
        assert!(
            store.selections.len() <= 500,
            "selections should be capped at MAX_QUERIES"
        );
    }

    #[test]
    fn prefix_matching_boosts_across_query_lengths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut store = Learning::load(&path);

        store.record("firefox", "firefox.desktop");
        store.record("firefox", "firefox.desktop");
        store.record("firefox", "firefox.desktop");

        // "fi" should match "firefox" via prefix matching
        let boost = store.query_boost("fi", "firefox.desktop");
        assert!(boost > 0, "prefix 'fi' should match learning for 'firefox'");

        // "firefox browser" should match "firefox" too (starts_with)
        let boost2 = store.query_boost("firefox browser", "firefox.desktop");
        assert!(boost2 > 0, "longer query should match stored shorter key");
    }

    #[test]
    fn recent_launches_sorted_by_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut store = Learning::load(&path);

        store.record("a", "first.desktop");
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.record("b", "second.desktop");

        let recent = store.recent_launches(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].0, "second.desktop", "most recent should be first");
        assert_eq!(recent[1].0, "first.desktop");
    }

    #[test]
    fn frequent_launches_excludes_specified_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut store = Learning::load(&path);

        for _ in 0..5 {
            store.record("a", "popular.desktop");
        }
        for _ in 0..2 {
            store.record("b", "other.desktop");
        }

        let frequent = store.frequent_launches(10, &["popular.desktop".to_string()]);
        assert!(
            frequent.iter().all(|(id, _)| id != "popular.desktop"),
            "excluded IDs should not appear"
        );
        assert!(!frequent.is_empty(), "should still have other entries");
    }

    // Adapted per decision 7: the salvage's `reset` cleared state *and*
    // persisted, using the path it stored on construction. `Learning` no
    // longer owns a path, so `reset` only clears in-memory state now; the
    // persistence half is asserted separately here via an explicit
    // save/reload.
    #[test]
    fn reset_clears_all_data_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut store = Learning::load(&path);

        store.record("test", "app.desktop");
        assert!(!store.is_empty());

        store.reset();
        assert!(store.is_empty());

        // The salvage's `reset` self-persisted; this one only clears
        // in-memory state, since `Learning` no longer owns a path to
        // persist to. Assert the persistence half explicitly instead.
        store.save(&path).unwrap();
        let loaded = Learning::load(&path);
        assert!(loaded.is_empty());
    }

    // --- New tests from the brief, written failing first. ---

    #[test]
    fn boost_capped_below_alias_boost() {
        let mut l = Learning::load(Path::new("/nonexistent"));
        for _ in 0..10_000 {
            l.record_launch("fire", &ItemId("app:firefox".into()));
        }
        let b = l.boost_for("fire", &ItemId("app:firefox".into()));
        assert!(b <= LEARNING_BOOST_CAP && b > 0.0);
        assert!(b < 180.0, "alias boost (180) must always beat learning");
    }

    #[test]
    fn corrupt_state_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("learning.json");
        std::fs::write(&p, "{not json").unwrap();
        let l = Learning::load(&p);
        assert_eq!(l.boost_for("x", &ItemId("y".into())), 0.0);
    }

    #[test]
    fn save_is_atomic_and_0600() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("learning.json");
        let mut l = Learning::load(&p);
        l.record_launch("q", &ItemId("app:a".into()));
        l.save(&p).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(Learning::load(&p).boost_for("q", &ItemId("app:a".into())) > 0.0);
    }

    // --- Coverage neither source reaches. ---

    #[test]
    fn missing_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("never-written.json");
        let l = Learning::load(&p);
        assert!(l.is_empty());
        assert_eq!(l.boost_for("anything", &ItemId("app:x".into())), 0.0);
    }

    #[test]
    fn wrong_shaped_json_loads_empty() {
        let dir = tempfile::tempdir().unwrap();

        let array_path = dir.path().join("array.json");
        std::fs::write(&array_path, "[1,2,3]").unwrap();
        let l = Learning::load(&array_path);
        assert!(l.is_empty());

        let bad_version_path = dir.path().join("bad-version.json");
        std::fs::write(&bad_version_path, r#"{"version":"not-a-number"}"#).unwrap();
        let l = Learning::load(&bad_version_path);
        assert!(l.is_empty());
    }

    #[test]
    fn temp_file_does_not_survive_a_successful_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut l = Learning::load(&path);
        l.record_launch("q", &ItemId("app:a".into()));
        l.save(&path).unwrap();

        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![path.file_name().unwrap().to_owned()]);
    }

    #[test]
    fn save_over_existing_file_replaces_rather_than_appends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");

        let mut first = Learning::load(&path);
        first.record_launch("q", &ItemId("app:a".into()));
        first.save(&path).unwrap();

        // Load fresh, wipe it, and record something entirely different. If
        // `save` appended rather than replaced the file, the reload below
        // would still carry "app:a"'s entry alongside the new one.
        let mut second = Learning::load(&path);
        second.reset();
        second.record_launch("other", &ItemId("app:b".into()));
        second.save(&path).unwrap();

        let reloaded = Learning::load(&path);
        assert_eq!(
            reloaded.frequency_boost("app:b"),
            second.frequency_boost("app:b"),
            "reloaded state should match the saver's"
        );
        assert_eq!(
            reloaded.frequency_boost("app:a"),
            0,
            "the first save's data should not survive being replaced by the second"
        );
    }

    #[test]
    fn boost_for_never_recorded_pairing_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut l = Learning::load(&path);
        l.record_launch("q", &ItemId("app:a".into()));

        // A pairing that was never recorded: "app:never-seen" has no
        // query-specific or global-frequency history at all.
        assert_eq!(l.boost_for("q", &ItemId("app:never-seen".into())), 0.0);
    }

    // `query_boost` early-returns zero for an empty (post-trim) query, but
    // `boost_for` sums in `frequency_boost`, which is query-independent by
    // design — it only looks up the item's global launch history, never the
    // query text. So an item *with* recorded launches, queried with an
    // empty string, returns its frequency boost rather than zero. This is
    // the real interaction the pipeline will meet in M1.7, when an empty
    // term returns everything ranked by weight and boost.
    #[test]
    fn boost_for_empty_query_still_carries_frequency_boost_for_a_recorded_item() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");
        let mut l = Learning::load(&path);
        l.record_launch("q", &ItemId("app:a".into()));

        let boost = l.boost_for("", &ItemId("app:a".into()));
        assert!(
            boost > 0.0,
            "frequency_boost doesn't consider the query text, so a recorded \
             item's boost survives an empty query"
        );
        assert_eq!(
            boost,
            l.frequency_boost("app:a") as f32,
            "with an empty query, boost_for is exactly the frequency component"
        );
    }

    // --- Parent-directory permissions (issue #36). ---
    //
    // These tests are about blast radius: `save` may narrow a directory it
    // created itself, and must not touch anything that was already there.
    // `persist_atomically`'s `DirBuilder` block says why.

    // A parent that already exists keeps whatever mode it had. 0755 is chosen
    // because it is both a plausible real-world mode and unmistakably not
    // 0700, so a stray chmod cannot pass this assertion by coincidence.
    #[cfg(unix)]
    #[test]
    fn save_leaves_a_pre_existing_parent_directory_mode_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("already-here");
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();

        let path = parent.join("learning.json");
        let mut l = Learning::load(&path);
        l.record_launch("q", &ItemId("app:a".into()));
        l.save(&path).unwrap();

        assert_eq!(
            std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
            0o755,
            "save must not re-permission a directory it did not create"
        );
    }

    // A parent this code creates is 0700, and so is every ancestor created
    // along the way — `~/.local/state` counts as "a directory this code
    // created" just as much as `~/.local/state/hop` does.
    //
    // There is deliberately no assertion about a transient wider mode: an
    // in-process test cannot observe one, and a timing loop would only
    // pretend to. So this test pins the post-condition only. The absence of
    // a window is structural rather than empirical, and lives at the code
    // that guarantees it — see `persist_atomically`'s comment on the
    // `mkdir(2)` mode argument.
    #[cfg(unix)]
    #[test]
    fn save_creates_missing_parent_directories_at_0700() {
        let dir = tempfile::tempdir().unwrap();
        let intermediate = dir.path().join("state");
        let leaf = intermediate.join("hop");
        let path = leaf.join("learning.json");

        let mut l = Learning::load(&path);
        l.record_launch("q", &ItemId("app:a".into()));
        l.save(&path).unwrap();

        assert_eq!(
            std::fs::metadata(&leaf).unwrap().permissions().mode() & 0o777,
            0o700,
            "a directory this code created must be owner-only"
        );
        assert_eq!(
            std::fs::metadata(&intermediate)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "an ancestor this code created is equally ours to narrow"
        );
    }

    // Regression guard for the chmod this fix removed: it followed the
    // symlink and narrowed the *target* — a directory somewhere else
    // entirely, which this code certainly did not create.
    #[cfg(unix)]
    #[test]
    fn save_does_not_chmod_through_a_symlinked_parent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real-directory");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();

        let link = dir.path().join("link-to-real-directory");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let path = link.join("learning.json");
        let mut l = Learning::load(&path);
        l.record_launch("q", &ItemId("app:a".into()));
        l.save(&path).unwrap();

        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755,
            "a symlinked parent's target must not be re-permissioned"
        );
    }

    // --- Serialization failure (issue #41). ---

    // A value `serde_json` genuinely refuses: JSON object keys must be
    // strings, and a tuple is not one. `PersistedLearningStore` cannot
    // produce such a value today, which is exactly why these tests enter at
    // `serialize_and_persist` — see its doc comment. Entering there is also
    // what makes them pin the ordering rather than assume it: the serialize
    // and every filesystem step below it are both inside the call.
    fn unserializable_value() -> HashMap<(u8, u8), u8> {
        let mut tuple_keyed = HashMap::new();
        tuple_keyed.insert((1, 2), 3);
        tuple_keyed
    }

    // Read the bytes, not the parsed store: the failure mode being guarded
    // against wrote an empty file, which parses back to an empty store —
    // indistinguishable from a legitimately empty one.
    #[test]
    fn a_serialization_failure_leaves_an_existing_store_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learning.json");

        let mut l = Learning::load(&path);
        l.record_launch("q", &ItemId("app:a".into()));
        l.save(&path).unwrap();
        let before = std::fs::read(&path).unwrap();

        assert!(serialize_and_persist(&path, &unserializable_value()).is_err());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "a value that cannot be serialized must not reach the store"
        );
    }

    // The path here is two components deep in an empty temp dir, so a
    // stray directory, a temp file or a destination file all show up as
    // something existing that should not. The missing parent directory is
    // the load-bearing part: it is the first syscall a save makes, so a
    // future edit that creates it before serializing turns this red.
    #[test]
    fn a_serialization_failure_creates_no_file_and_no_directory() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("state");
        let path = parent.join("learning.json");

        assert!(serialize_and_persist(&path, &unserializable_value()).is_err());

        assert!(
            !parent.exists(),
            "the parent directory should not be created"
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "no destination file and no temp file should be left behind"
        );
    }

    #[test]
    fn save_propagates_error_when_parent_cannot_be_created() {
        let dir = tempfile::tempdir().unwrap();
        let blocking_file = dir.path().join("not-a-directory");
        std::fs::write(&blocking_file, b"i am a file, not a directory").unwrap();
        let path = blocking_file.join("learning.json");

        let l = Learning::load(&path);
        assert!(l.save(&path).is_err());
    }
}
