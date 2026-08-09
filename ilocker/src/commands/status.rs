// ============================================================
//  commands/status.rs — iloc status  (Phase 2 update)
// ============================================================

use crate::commands::init::assert_initialised;
use crate::db;
use crate::snapshot;
use crate::utils::{db_path, human_bytes, IgnoreRules};
use anyhow::Result;
use colored::Colorize;

pub fn run() -> Result<()> {
    let ilocker_dir  = assert_initialised()?;
    let project_root = ilocker_dir.parent().unwrap().to_path_buf();
    let db_file      = db_path(&ilocker_dir);
    let conn         = db::open(&db_file)?;

    let ignore = IgnoreRules::load(&project_root);

    println!("{}", "Scanning…".dimmed());
    let (current, _stats) = snapshot::scan_project(&project_root, &ignore)?;

    let (previous, snap_info) = match db::latest_snapshot(&conn)? {
        Some(snap) => {
            let records = db::files_for_snapshot(&conn, &snap.id)?;
            (records, Some(snap))
        }
        None => (Vec::new(), None),
    };

    let diff = snapshot::diff(&current, &previous);

    println!();
    match &snap_info {
        Some(s) => println!(
            "{} vs snapshot {} — \"{}\"",
            "Status".bold(), s.id.cyan(), s.message
        ),
        None => println!("{} (no previous snapshot)", "Status".bold()),
    }
    println!();

    if !diff.added.is_empty() {
        println!("  {} {} file(s):", "A".green().bold(), diff.added.len());
        for f in &diff.added {
            println!("    {} {}", "+".green(), f.rel_path.display());
        }
        println!();
    }
    if !diff.modified.is_empty() {
        println!("  {} {} file(s):", "M".yellow().bold(), diff.modified.len());
        for f in &diff.modified {
            println!("    {} {}", "~".yellow(), f.rel_path.display());
        }
        println!();
    }
    if !diff.deleted.is_empty() {
        println!("  {} {} file(s):", "D".red().bold(), diff.deleted.len());
        for p in &diff.deleted {
            println!("    {} {}", "-".red(), p);
        }
        println!();
    }

    println!("  {} {} file(s) unchanged", "=".dimmed(), diff.unchanged.len());

    let total_bytes: u64 = current.iter().map(|f| f.size_bytes).sum();
    println!();
    println!("  total {} files · {}", current.len().to_string().bold(), human_bytes(total_bytes).bold());

    let dirty = diff.added.len() + diff.modified.len() + diff.deleted.len();
    if dirty > 0 {
        println!();
        println!("{}", format!("  {} change(s) detected.  Run `iloc save \"<message>\"` to snapshot.", dirty).dimmed());
    } else {
        println!("{}", "  Working tree is clean.".green());
    }

    Ok(())
}
