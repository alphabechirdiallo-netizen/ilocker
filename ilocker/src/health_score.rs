// ============================================================
//  health_score.rs — Project Health Score (Phase 5A Feature 3)
//
//  Computes a structural health score (0–100) for a project
//  by analysing its snapshot history, file composition, and
//  growth trends.  Entirely local — no network required.
//
//  Score breakdown:
//    snapshot_frequency  (0–25)  — how regularly the dev saves
//    delta_efficiency    (0–25)  — how small the average delta is
//    binary_hygiene      (0–25)  — absence of large committed binaries
//    history_cleanliness (0–25)  — no bloated snapshots over time
//
//  Total: 100 points  →  grade A (90+), B (75+), C (60+), D (<60)
// ============================================================

use crate::db::{self, Snapshot};
use crate::utils::human_bytes;
use anyhow::Result;
use chrono::{DateTime, Utc};
use colored::Colorize;
use std::path::Path;

// ── Thresholds ────────────────────────────────────────────────

/// Files larger than this are flagged as "large binary" suspects (50 MiB)
const LARGE_FILE_THRESHOLD_BYTES: i64 = 50 * 1024 * 1024;

/// Ideal snapshot frequency: at least once per 3 days
const IDEAL_SNAPSHOT_INTERVAL_DAYS: i64 = 3;

/// Maximum healthy delta ratio (changed bytes / total bytes)
const HEALTHY_DELTA_RATIO: f64 = 0.30;

/// Alert when a single snapshot exceeds this size (500 MiB)
const BLOAT_THRESHOLD_BYTES: i64 = 500 * 1024 * 1024;

// ── Public API ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct HealthReport {
    pub total_score:          u8,    // 0–100
    pub grade:                char,  // A B C D
    pub snapshot_score:       u8,    // 0–25
    pub delta_score:          u8,    // 0–25
    pub binary_score:         u8,    // 0–25
    pub history_score:        u8,    // 0–25
    pub snapshot_count:       usize,
    pub total_snapshots_bytes:u64,
    pub large_files:          Vec<LargeFileAlert>,
    pub bloated_snapshots:    Vec<BloatAlert>,
    pub recommendations:      Vec<Recommendation>,
    pub avg_delta_bytes:      u64,
    pub days_since_last_save: i64,
}

#[derive(Debug, Clone)]
pub struct LargeFileAlert {
    pub rel_path:   String,
    pub size_bytes: u64,
    pub snapshot_id:String,
}

#[derive(Debug, Clone)]
pub struct BloatAlert {
    pub snapshot_id: String,
    pub message:     String,
    pub total_bytes: u64,
}

#[derive(Debug, Clone)]
pub enum Recommendation {
    AddToIgnore   { pattern: String, reason: String },
    SaveMoreOften { current_interval_days: i64 },
    CleanHistory  { bloated_snaps: usize },
    SplitProject  { reason: String },
}

// ── Score computation ─────────────────────────────────────────

pub fn compute(db_file: &Path) -> Result<HealthReport> {
    let conn      = db::open(db_file)?;
    let snapshots = db::list_snapshots(&conn)?;

    if snapshots.is_empty() {
        return Ok(zero_score_report("No snapshots yet — run `iloc save` to begin tracking."));
    }

    // ── 1. Snapshot frequency score ───────────────────────────
    let (snapshot_score, days_since_last) = score_frequency(&snapshots);

    // ── 2. Delta efficiency score ─────────────────────────────
    let (delta_score, avg_delta) = score_delta_efficiency(&snapshots);

    // ── 3. Binary hygiene score ───────────────────────────────
    let (binary_score, large_files) = score_binary_hygiene(&conn, &snapshots)?;

    // ── 4. History cleanliness score ──────────────────────────
    let (history_score, bloated_snaps, total_bytes) =
        score_history_cleanliness(&snapshots);

    let total_score = snapshot_score + delta_score + binary_score + history_score;
    let grade = score_to_grade(total_score);

    // ── 5. Recommendations ───────────────────────────────────
    let recommendations = build_recommendations(
        days_since_last, &large_files, &bloated_snaps, avg_delta,
    );

    Ok(HealthReport {
        total_score,
        grade,
        snapshot_score,
        delta_score,
        binary_score,
        history_score,
        snapshot_count:        snapshots.len(),
        total_snapshots_bytes: total_bytes,
        large_files,
        bloated_snapshots:     bloated_snaps,
        recommendations,
        avg_delta_bytes:       avg_delta,
        days_since_last_save:  days_since_last,
    })
}

fn score_frequency(snapshots: &[Snapshot]) -> (u8, i64) {
    // Most recent snapshot
    let latest = &snapshots[0];
    let latest_dt: DateTime<Utc> = latest.created_at
        .parse()
        .unwrap_or_else(|_| Utc::now());
    let days_since = (Utc::now() - latest_dt).num_days().max(0);

    let score = if days_since == 0 {
        25u8
    } else if days_since <= IDEAL_SNAPSHOT_INTERVAL_DAYS {
        22
    } else if days_since <= 7 {
        15
    } else if days_since <= 14 {
        8
    } else {
        2
    };

    (score, days_since)
}

fn score_delta_efficiency(snapshots: &[Snapshot]) -> (u8, u64) {
    if snapshots.len() < 2 {
        return (25, 0);
    }

    // Compute average delta: difference in total_bytes between consecutive snaps
    let mut deltas: Vec<u64> = Vec::new();
    for window in snapshots.windows(2) {
        let newer = &window[0];
        let older = &window[1];
        let delta = (newer.total_bytes - older.total_bytes).unsigned_abs();
        deltas.push(delta);
    }

    let avg_delta = deltas.iter().sum::<u64>() / deltas.len() as u64;
    let latest_total = snapshots[0].total_bytes.max(1) as u64;
    let ratio = avg_delta as f64 / latest_total as f64;

    let score = if ratio < 0.05 {
        25u8  // < 5% change on average — excellent
    } else if ratio < HEALTHY_DELTA_RATIO {
        18
    } else if ratio < 0.60 {
        10
    } else {
        4  // > 60% change each time — probably committing large binaries
    };

    (score, avg_delta)
}

fn score_binary_hygiene(
    conn:      &rusqlite::Connection,
    snapshots: &[Snapshot],
) -> Result<(u8, Vec<LargeFileAlert>)> {
    if snapshots.is_empty() {
        return Ok((25, vec![]));
    }

    // Check the latest snapshot's file index for large files
    let latest_id = &snapshots[0].id;
    let records   = db::files_for_snapshot(conn, latest_id)?;

    let large_files: Vec<LargeFileAlert> = records
        .iter()
        .filter(|r| r.size_bytes > LARGE_FILE_THRESHOLD_BYTES)
        .map(|r| LargeFileAlert {
            rel_path:    r.rel_path.clone(),
            size_bytes:  r.size_bytes as u64,
            snapshot_id: latest_id.clone(),
        })
        .collect();

    let score = match large_files.len() {
        0          => 25u8,
        1          => 18,
        2..=3      => 10,
        _          => 3,
    };

    Ok((score, large_files))
}

fn score_history_cleanliness(
    snapshots: &[Snapshot],
) -> (u8, Vec<BloatAlert>, u64) {
    let total_bytes: u64 = snapshots.iter()
        .map(|s| s.total_bytes as u64)
        .sum();

    let bloated: Vec<BloatAlert> = snapshots.iter()
        .filter(|s| s.total_bytes > BLOAT_THRESHOLD_BYTES)
        .map(|s| BloatAlert {
            snapshot_id: s.id.clone(),
            message:     format!(
                "Snapshot '{}' is very large ({})",
                &s.message[..s.message.len().min(40)],
                human_bytes(s.total_bytes as u64)
            ),
            total_bytes: s.total_bytes as u64,
        })
        .collect();

    let score = match bloated.len() {
        0     => 25u8,
        1     => 18,
        2..=3 => 10,
        _     => 4,
    };

    (score, bloated, total_bytes)
}

fn score_to_grade(score: u8) -> char {
    match score {
        90..=100 => 'A',
        75..=89  => 'B',
        60..=74  => 'C',
        _        => 'D',
    }
}

fn build_recommendations(
    days_since:    i64,
    large_files:   &[LargeFileAlert],
    bloated_snaps: &[BloatAlert],
    avg_delta:     u64,
) -> Vec<Recommendation> {
    let mut recs = Vec::new();

    if days_since > IDEAL_SNAPSHOT_INTERVAL_DAYS {
        recs.push(Recommendation::SaveMoreOften {
            current_interval_days: days_since,
        });
    }

    for lf in large_files {
        // Suggest ignoring the parent directory or extension
        let pattern = if lf.rel_path.contains('/') {
            lf.rel_path.split('/').next().unwrap_or(&lf.rel_path).to_string() + "/"
        } else {
            format!("*.{}", lf.rel_path.split('.').last().unwrap_or("bin"))
        };
        recs.push(Recommendation::AddToIgnore {
            pattern,
            reason: format!(
                "{} is {} — consider adding to .ilockerignore",
                lf.rel_path, human_bytes(lf.size_bytes)
            ),
        });
    }

    if !bloated_snaps.is_empty() {
        recs.push(Recommendation::CleanHistory {
            bloated_snaps: bloated_snaps.len(),
        });
    }

    // Large average delta suggests a modular split might help
    if avg_delta > 100 * 1024 * 1024 {
        recs.push(Recommendation::SplitProject {
            reason: format!(
                "Average delta of {} per snapshot is high — consider splitting datasets into a separate project",
                human_bytes(avg_delta)
            ),
        });
    }

    recs
}

fn zero_score_report(_note: &str) -> HealthReport {
    HealthReport {
        total_score: 0, grade: 'D',
        snapshot_score: 0, delta_score: 0, binary_score: 0, history_score: 0,
        snapshot_count: 0, total_snapshots_bytes: 0,
        large_files: vec![], bloated_snapshots: vec![],
        recommendations: vec![],
        avg_delta_bytes: 0, days_since_last_save: 0,
    }
}

// ── Display ───────────────────────────────────────────────────

pub fn print_health_report(report: &HealthReport) {
    println!();
    println!("{}", "── Project Health Score ─────────────────────────────────".dimmed());
    println!();

    // Grade badge
    let grade_colored = match report.grade {
        'A' => format!(" {} ", report.grade).black().on_green().bold(),
        'B' => format!(" {} ", report.grade).black().on_cyan().bold(),
        'C' => format!(" {} ", report.grade).black().on_yellow().bold(),
        _   => format!(" {} ", report.grade).white().on_red().bold(),
    };
    println!(
        "  {} {}  {}/100",
        "Score:".dimmed(),
        grade_colored,
        report.total_score.to_string().bold()
    );
    println!();

    // Sub-scores
    bar_row("Snapshot frequency", report.snapshot_score, 25);
    bar_row("Delta efficiency",   report.delta_score,    25);
    bar_row("Binary hygiene",     report.binary_score,   25);
    bar_row("History health",     report.history_score,  25);

    println!();
    println!("  {} {} snapshots · {} last save {} days ago",
        "History:".dimmed(),
        report.snapshot_count.to_string().yellow(),
        "·".dimmed(),
        report.days_since_last_save
    );

    // Large file alerts
    if !report.large_files.is_empty() {
        println!();
        println!("  {} Large files detected:", "⚠".yellow().bold());
        for lf in &report.large_files {
            println!(
                "    {} {}  ({})",
                "●".red(),
                lf.rel_path.bold(),
                human_bytes(lf.size_bytes)
            );
        }
    }

    // Bloat alerts
    if !report.bloated_snapshots.is_empty() {
        println!();
        println!("  {} Bloated snapshots:", "⚠".yellow().bold());
        for bs in &report.bloated_snapshots {
            println!("    {} {}", "●".yellow(), bs.message);
        }
    }

    // Recommendations
    if !report.recommendations.is_empty() {
        println!();
        println!("  {}", "Recommendations:".bold());
        for rec in &report.recommendations {
            match rec {
                Recommendation::SaveMoreOften { current_interval_days } => {
                    println!(
                        "    {} Save more frequently (last save was {} days ago)",
                        "→".cyan(), current_interval_days
                    );
                }
                Recommendation::AddToIgnore { pattern, reason } => {
                    println!("    {} Add {} to .ilockerignore — {}",
                        "→".cyan(), pattern.yellow(), reason.dimmed());
                }
                Recommendation::CleanHistory { bloated_snaps } => {
                    println!(
                        "    {} {} large snapshot(s) detected — review and prune history",
                        "→".cyan(), bloated_snaps
                    );
                }
                Recommendation::SplitProject { reason } => {
                    println!("    {} {}", "→".cyan(), reason.dimmed());
                }
            }
        }
    }
    println!();
}

fn bar_row(label: &str, score: u8, max: u8) {
    let filled   = (score as usize * 20) / max as usize;
    let empty    = 20usize.saturating_sub(filled);
    let bar_filled = "█".repeat(filled);
    let bar_empty  = "░".repeat(empty);
    println!(
        "  {:<22} [{}{bar_empty}] {}/{}",
        label.dimmed(),
        bar_filled.green(),
        score,
        max
    );
}
