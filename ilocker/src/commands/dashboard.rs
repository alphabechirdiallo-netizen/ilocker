// ============================================================
//  commands/dashboard.rs — iloc dashboard
//
//  A rich, interactive terminal dashboard built with crossterm.
//  Design language: flat, professional, monochrome structure
//  with strategic color accents — inspired by modern DevTools.
//
//  Layout
//  ──────
//  ┌─────────────────────────────────────────────────────────┐
//  │  LOGO (pixel-art, true-color on capable terminals)      │
//  │  ilocker v1.0.0                                         │
//  ├──────────────────────┬──────────────────────────────────┤
//  │  PROJECTS            │  SNAPSHOT TREE                   │
//  │  (scrollable list)   │  (for selected project)          │
//  ├──────────────────────┴──────────────────────────────────┤
//  │  STORAGE STATS        ── dedup savings ──               │
//  ├─────────────────────────────────────────────────────────┤
//  │  [Q] Quit  [↑↓] Navigate  [Enter] Select               │
//  └─────────────────────────────────────────────────────────┘
//
//  Keyboard
//  ────────
//  q / Esc   → quit
//  ↑ / k     → previous project
//  ↓ / j     → next project
//  Enter      → expand / collapse snapshot tree
// ============================================================

use crate::db;
use crate::logo::{LOGO_COLORED, LOGO_PLAIN};
use crate::utils::{db_path, human_bytes};
use anyhow::Result;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute, queue,
    style::{Color, SetBackgroundColor, SetForegroundColor, ResetColor, Attribute, SetAttribute},
    terminal::{self, ClearType},
};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

// ── Color palette ─────────────────────────────────────────────
// Flat, professional — inspired by VS Code dark theme
const C_BG:        Color = Color::Black;
const C_PANEL:     Color = Color::Rgb { r: 20,  g: 20,  b: 24  };
const C_BORDER:    Color = Color::Rgb { r: 50,  g: 50,  b: 58  };
const C_TEXT:      Color = Color::Rgb { r: 200, g: 200, b: 205 };
const C_DIM:       Color = Color::Rgb { r: 100, g: 100, b: 108 };
const C_ACCENT:    Color = Color::Rgb { r: 80,  g: 180, b: 255 };  // iloc blue
const C_GREEN:     Color = Color::Rgb { r: 80,  g: 210, b: 120 };
const C_YELLOW:    Color = Color::Rgb { r: 255, g: 200, b: 80  };
#[allow(dead_code)]
const C_ORANGE:    Color = Color::Rgb { r: 255, g: 140, b: 60  };
const C_SELECT_BG: Color = Color::Rgb { r: 30,  g: 50,  b: 80  };

// ── Project info ──────────────────────────────────────────────

#[derive(Debug)]
struct ProjectInfo {
    path:          PathBuf,
    name:          String,
    project_key:   String,
    snapshot_count: usize,
    snapshots:     Vec<db::Snapshot>,
    logical_bytes: u64,   // sum of all snapshot file sizes (without dedup)
    actual_bytes:  u64,   // real disk usage of .ilocker/
}

// ── Entry point ───────────────────────────────────────────────

pub fn run() -> Result<()> {
    let projects = discover_projects()?;

    if projects.is_empty() {
        println!();
        println!("  No ilocker projects found on this machine.");
        println!("  Run `iloc init` inside a project directory to get started.");
        println!();
        return Ok(());
    }

    // Le logo occupe à lui seul ~31 lignes ; en dessous d'un certain nombre
    // de lignes disponibles, le panneau de contenu (projets/snapshots) n'a
    // plus de place du tout. Plutôt que de dessiner un dashboard tronqué
    // à 0 ligne de contenu (ou de risquer un dépassement arithmétique sur
    // un calcul de largeur/hauteur dérivé), on prévient clairement l'
    // utilisateur AVANT d'entrer en mode raw/alternate-screen.
    const MIN_ROWS: u16 = 44;
    const MIN_COLS: u16 = 70;
    let (cols, rows) = terminal::size()?;
    if rows < MIN_ROWS || cols < MIN_COLS {
        println!();
        println!(
            "  Terminal trop petit pour le dashboard ({}x{}, minimum {}x{}).",
            cols, rows, MIN_COLS, MIN_ROWS
        );
        println!("  Agrandissez la fenêtre du terminal, puis relancez `iloc dashboard`.");
        println!();
        return Ok(());
    }

    // Check terminal color support
    let true_color = supports_true_color();

    // Filet de sécurité : si le rendu panique pour une raison quelconque
    // (bug non prévu ici, terminal exotique, etc.), on restaure quand même
    // le terminal AVANT d'afficher le message de panic. Sans ce hook, un
    // panic en plein mode raw + alternate-screen laisse le terminal de
    // l'utilisateur dans un état cassé (raw mode actif, mauvais écran) —
    // ce qui ressemble exactement à un affichage "gribouillé" et persiste
    // même après la fin du process.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, terminal::LeaveAlternateScreen, cursor::Show);
        let _ = terminal::disable_raw_mode();
        default_hook(info);
    }));

    // Enter raw/alternate-screen mode
    let mut stdout = io::stdout();
    terminal::enable_raw_mode()?;
    execute!(
        stdout,
        terminal::EnterAlternateScreen,
        cursor::Hide,
    )?;

    let result = run_loop(&mut stdout, &projects, true_color);

    // Always restore terminal even on error
    execute!(
        stdout,
        terminal::LeaveAlternateScreen,
        cursor::Show,
    )?;
    terminal::disable_raw_mode()?;

    // Restaurer le hook par défaut : sinon toute panique APRÈS le dashboard
    // (dans une commande complètement différente lancée plus tard dans le
    // même process) continuerait inutilement à tenter de "nettoyer" un
    // terminal qui n'est plus en mode raw.
    let _ = std::panic::take_hook();

    result
}

// ── Main event loop ───────────────────────────────────────────

fn run_loop(
    stdout:     &mut io::Stdout,
    projects:   &[ProjectInfo],
    true_color: bool,
) -> Result<()> {
    let mut selected   = 0usize;
    let mut expanded   = true;   // show snapshot tree by default

    loop {
        let (cols, rows) = terminal::size()?;
        draw(stdout, projects, selected, expanded, true_color, cols, rows)?;

        // Poll with 50 ms timeout for smooth rendering
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(KeyEvent { code, modifiers, .. }) => {
                    match code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => break,

                        KeyCode::Up | KeyCode::Char('k') => {
                            if selected > 0 { selected -= 1; }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if selected < projects.len().saturating_sub(1) {
                                selected += 1;
                            }
                        }

                        KeyCode::Enter | KeyCode::Char(' ') => {
                            expanded = !expanded;
                        }

                        _ => {}
                    }
                }
                Event::Resize(_, _) => {
                    // Redraw on resize — handled by loop
                    execute!(stdout, terminal::Clear(ClearType::All))?;
                }
                _ => {}
            }
        }
    }

    Ok(())
}

// ── Rendering ─────────────────────────────────────────────────

fn draw(
    stdout:     &mut io::Stdout,
    projects:   &[ProjectInfo],
    selected:   usize,
    expanded:   bool,
    true_color: bool,
    cols:       u16,
    rows:       u16,
) -> Result<()> {
    queue!(stdout, cursor::MoveTo(0, 0))?;

    let logo_lines: &[&str] = if true_color { LOGO_COLORED } else { LOGO_PLAIN };
    let logo_h = logo_lines.len() as u16;

    // ── 1. Logo + title bar ───────────────────────────────────
    draw_logo(stdout, logo_lines, cols, true_color)?;
    draw_title_bar(stdout, logo_h, cols)?;

    let content_top = logo_h + 2; // logo + title + separator

    // ── 2. Main content area ──────────────────────────────────
    let stats_h   = 4u16;
    let footer_h  = 2u16;
    let content_h = rows.saturating_sub(content_top + stats_h + footer_h);

    let left_w  = (cols / 3).max(28);
    let right_w = cols.saturating_sub(left_w + 1);

    // Left panel: project list
    draw_panel_border(stdout, content_top, 0, left_w, content_h, "  PROJECTS", cols)?;
    draw_project_list(stdout, projects, selected, content_top + 1, 1, left_w.saturating_sub(2), content_h.saturating_sub(2))?;

    // Right panel: snapshot tree
    draw_panel_border(stdout, content_top, left_w, right_w, content_h, "  SNAPSHOT HISTORY", cols)?;
    if let Some(proj) = projects.get(selected) {
        draw_snapshot_tree(
            stdout, proj, expanded,
            content_top + 1, left_w + 1, right_w.saturating_sub(2), content_h.saturating_sub(2),
        )?;
    }

    // ── 3. Stats bar ──────────────────────────────────────────
    let stats_top = content_top + content_h;
    draw_stats_bar(stdout, projects, stats_top, cols)?;

    // ── 4. Footer ─────────────────────────────────────────────
    draw_footer(stdout, rows.saturating_sub(1), cols)?;

    stdout.flush()?;
    Ok(())
}

// ── Logo rendering ────────────────────────────────────────────

fn draw_logo(
    stdout:     &mut io::Stdout,
    lines:      &[&str],
    cols:       u16,
    true_color: bool,
) -> Result<()> {
    let logo_w = 68u16 * 2; // each block is 2 chars
    let pad    = ((cols as i32 - logo_w as i32) / 2).max(0) as u16;

    for (i, line) in lines.iter().enumerate() {
        queue!(
            stdout,
            cursor::MoveTo(pad, i as u16),
        )?;
        if true_color {
            // Line already contains ANSI escape sequences
            write!(stdout, "{}", line)?;
        } else {
            // Plain blocks — use dim cyan for monochrome terminals
            queue!(stdout, SetForegroundColor(Color::Rgb { r: 60, g: 140, b: 200 }))?;
            write!(stdout, "{}", line)?;
            queue!(stdout, ResetColor)?;
        }
    }
    Ok(())
}

// ── Title bar ─────────────────────────────────────────────────

fn draw_title_bar(stdout: &mut io::Stdout, row: u16, cols: u16) -> Result<()> {
    queue!(
        stdout,
        cursor::MoveTo(0, row),
        SetBackgroundColor(C_PANEL),
        SetForegroundColor(C_ACCENT),
        SetAttribute(Attribute::Bold),
    )?;

    let title = "  ilocker  v1.0.0";
    let subtitle = "  Instant snapshot engine  |  Zero-Knowledge P2P sharing  ";
    let right_side = format!("{:>width$}", subtitle, width = (cols as usize).saturating_sub(title.len()));
    let full = format!("{}{}", title, right_side);
    let padded = format!("{:<width$}", truncate(&full, cols as usize), width = cols as usize);
    write!(stdout, "{}", padded)?;

    queue!(
        stdout,
        cursor::MoveTo(0, row + 1),
        SetBackgroundColor(C_BG),
        SetForegroundColor(C_BORDER),
        ResetColor,
        SetAttribute(Attribute::Reset),
    )?;
    write!(stdout, "{}", "\u{2500}".repeat(cols as usize))?;

    Ok(())
}

// ── Panel border ──────────────────────────────────────────────

fn draw_panel_border(
    stdout: &mut io::Stdout,
    top:    u16,
    left:   u16,
    width:  u16,
    height: u16,
    label:  &str,
    _cols:  u16,
) -> Result<()> {
    // Top border with label
    queue!(
        stdout,
        cursor::MoveTo(left, top),
        SetForegroundColor(C_BORDER),
    )?;
    let lbl = format!("{}{}", label, " ");
    // Largeur totale exacte : ┌(1) + ─(1) + lbl + (N × ─) + ┐(1) = width
    // => N = width - lbl.len() - 3. L'ancien calcul ('remaining = width -
    // lbl.len() - 2') oubliait à la fois le ┐ final ET un espace littéral
    // laissé dans le format string : la bordure du haut débordait donc de
    // 2 colonnes dans le panneau voisin (confirmé visuellement en rejouant
    // le dashboard via un émulateur de terminal réel).
    let remaining = (width as usize).saturating_sub(lbl.len() + 3);
    write!(
        stdout,
        "{}{}{}{}{}",
        "\u{250C}", "\u{2500}",
        lbl,
        "\u{2500}".repeat(remaining),
        "\u{2510}"
    )?;

    // Side borders
    for r in 1..height {
        queue!(stdout, cursor::MoveTo(left, top + r), SetForegroundColor(C_BORDER))?;
        write!(stdout, "\u{2502}")?;
        queue!(stdout, cursor::MoveTo(left + width.saturating_sub(1), top + r), SetForegroundColor(C_BORDER))?;
        write!(stdout, "\u{2502}")?;
    }

    // Bottom border
    queue!(stdout, cursor::MoveTo(left, top + height), SetForegroundColor(C_BORDER))?;
    write!(stdout, "{}{}{}", "\u{2514}", "\u{2500}".repeat(width.saturating_sub(2) as usize), "\u{2518}")?;

    queue!(stdout, ResetColor)?;
    Ok(())
}

// ── Project list ──────────────────────────────────────────────

fn draw_project_list(
    stdout:   &mut io::Stdout,
    projects: &[ProjectInfo],
    selected: usize,
    top:      u16,
    left:     u16,
    width:    u16,
    height:   u16,
) -> Result<()> {
    for (i, proj) in projects.iter().enumerate().take(height as usize) {
        let row = top + i as u16;

        if i == selected {
            queue!(
                stdout,
                cursor::MoveTo(left, row),
                SetBackgroundColor(C_SELECT_BG),
                SetForegroundColor(C_ACCENT),
                SetAttribute(Attribute::Bold),
            )?;
        } else {
            queue!(
                stdout,
                cursor::MoveTo(left, row),
                SetBackgroundColor(C_BG),
                SetForegroundColor(C_TEXT),
                SetAttribute(Attribute::Reset),
            )?;
        }

        let snap_count = format!(" [{}]", proj.snapshot_count);
        let name_w = (width as usize).saturating_sub(snap_count.len() + 2);
        let name   = truncate(&proj.name, name_w);
        let line   = format!(" {:<nw$}{}", name, snap_count, nw = name_w);
        let padded = format!("{:<width$}", truncate(&line, width as usize), width = width as usize);
        write!(stdout, "{}", padded)?;

        // Second line: dim path
        if i < height as usize / 2 {  // only if space
            let path_row = row;
            if i == selected {
                queue!(stdout, cursor::MoveTo(left, path_row), SetForegroundColor(C_ACCENT))?;
            } else {
                queue!(stdout, cursor::MoveTo(left, path_row), SetForegroundColor(C_DIM))?;
            }
        }
    }

    queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

// ── Snapshot tree ─────────────────────────────────────────────

fn draw_snapshot_tree(
    stdout:   &mut io::Stdout,
    proj:     &ProjectInfo,
    expanded: bool,
    top:      u16,
    left:     u16,
    width:    u16,
    height:   u16,
) -> Result<()> {
    if proj.snapshots.is_empty() {
        queue!(stdout, cursor::MoveTo(left + 2, top + 1), SetForegroundColor(C_DIM))?;
        write!(stdout, "No snapshots yet.")?;
        return Ok(());
    }

    // Project key in accent
    queue!(
        stdout,
        cursor::MoveTo(left + 1, top),
        SetForegroundColor(C_DIM),
    )?;
    let key_line = truncate(&proj.project_key, (width as usize).saturating_sub(2));
    write!(stdout, " {}", key_line)?;

    // Path
    queue!(stdout, cursor::MoveTo(left + 1, top + 1), SetForegroundColor(C_DIM))?;
    let path_str = truncate(&proj.path.display().to_string(), (width as usize).saturating_sub(2));
    write!(stdout, " {}", path_str)?;

    if !expanded {
        queue!(stdout, cursor::MoveTo(left + 1, top + 2), SetForegroundColor(C_DIM))?;
        write!(stdout, " [Enter] to expand snapshot tree")?;
        return Ok(());
    }

    // Snapshot tree
    let tree_top = top + 2;
    let avail    = height.saturating_sub(3) as usize;
    let snaps    = &proj.snapshots[..proj.snapshots.len().min(avail)];

    for (i, snap) in snaps.iter().enumerate() {
        let row      = tree_top + i as u16;
        let is_last  = i == snaps.len() - 1;
        let is_first = i == 0;

        // Tree connector
        let connector = if is_last { "\u{2514}\u{2500}" } else { "\u{251C}\u{2500}" };

        // Timestamp (short)
        let ts = &snap.created_at[..16];  // "YYYY-MM-DDTHH:MM"
        let ts_display = ts.replace('T', " ");

        // Colour the latest snap differently
        if is_first {
            queue!(stdout, cursor::MoveTo(left + 1, row), SetForegroundColor(C_GREEN))?;
        } else {
            queue!(stdout, cursor::MoveTo(left + 1, row), SetForegroundColor(C_TEXT))?;
        }

        let id_short = &snap.id[..8];
        let msg = truncate(&snap.message, (width as usize).saturating_sub(35));
        let files_label = format!("{} files", snap.file_count);

        let line = format!(
            " {} {} {} {}  {}  {}",
            connector,
            if is_first { "*" } else { " " },
            id_short,
            msg,
            ts_display,
            files_label,
        );
        let padded = format!("{:<w$}", truncate(&line, width as usize), w = width as usize);
        write!(stdout, "{}", padded)?;
    }

    if proj.snapshots.len() > avail {
        let more_row = tree_top + avail as u16;
        queue!(stdout, cursor::MoveTo(left + 1, more_row), SetForegroundColor(C_DIM))?;
        write!(stdout, "   ... {} more snapshots", proj.snapshots.len() - avail)?;
    }

    queue!(stdout, ResetColor)?;
    Ok(())
}

// ── Stats bar ─────────────────────────────────────────────────

fn draw_stats_bar(
    stdout:   &mut io::Stdout,
    projects: &[ProjectInfo],
    top:      u16,
    cols:     u16,
) -> Result<()> {
    let total_logical: u64 = projects.iter().map(|p| p.logical_bytes).sum();
    let total_actual:  u64 = projects.iter().map(|p| p.actual_bytes).sum();
    let saved          = total_logical.saturating_sub(total_actual);
    let ratio          = if total_logical > 0 {
        100.0 * saved as f64 / total_logical as f64
    } else {
        0.0
    };

    // Separator
    queue!(
        stdout,
        cursor::MoveTo(0, top),
        SetForegroundColor(C_BORDER),
    )?;
    write!(stdout, "{}", "\u{2500}".repeat(cols as usize))?;

    // Row 1: headline numbers
    queue!(
        stdout,
        cursor::MoveTo(0, top + 1),
        SetBackgroundColor(C_PANEL),
        SetForegroundColor(C_DIM),
    )?;
    write!(stdout, "{:<width$}", "", width = cols as usize)?;

    let col1 = format!(
        "  Projects: {}",
        projects.len()
    );
    let col2 = format!(
        "  Snapshots: {}",
        projects.iter().map(|p| p.snapshot_count).sum::<usize>()
    );
    let col3 = format!(
        "  Logical: {}",
        human_bytes(total_logical)
    );
    let col4 = format!(
        "  Actual: {}",
        human_bytes(total_actual)
    );

    queue!(stdout, cursor::MoveTo(0, top + 1), SetForegroundColor(C_TEXT), SetBackgroundColor(C_PANEL))?;
    write!(stdout, "{}", col1)?;
    queue!(stdout, SetForegroundColor(C_DIM))?;
    write!(stdout, "{}", col2)?;
    queue!(stdout, SetForegroundColor(C_TEXT))?;
    write!(stdout, "{}", col3)?;
    queue!(stdout, SetForegroundColor(C_YELLOW))?;
    write!(stdout, "{}", col4)?;

    // Row 2: savings bar
    queue!(
        stdout,
        cursor::MoveTo(0, top + 2),
        SetBackgroundColor(C_PANEL),
        SetForegroundColor(C_DIM),
    )?;
    write!(stdout, "{:<width$}", "", width = cols as usize)?;

    queue!(stdout, cursor::MoveTo(0, top + 2))?;
    let bar_label = format!(
        "  Saved by deduplication: {} ({:.1}%)",
        human_bytes(saved), ratio
    );
    queue!(stdout, SetForegroundColor(C_GREEN), SetAttribute(Attribute::Bold))?;
    write!(stdout, "{}", bar_label)?;

    // Visual bar
    let bar_start = bar_label.len() + 2;
    let bar_avail = (cols as usize).saturating_sub(bar_start + 4);
    let filled    = (bar_avail as f64 * ratio / 100.0) as usize;

    queue!(stdout, cursor::MoveTo(bar_start as u16, top + 2))?;
    queue!(stdout, SetForegroundColor(C_BORDER))?;
    write!(stdout, "[")?;
    queue!(stdout, SetForegroundColor(C_GREEN))?;
    write!(stdout, "{}", "\u{2588}".repeat(filled))?;
    queue!(stdout, SetForegroundColor(Color::Rgb { r: 40, g: 60, b: 40 }))?;
    write!(stdout, "{}", "\u{2591}".repeat(bar_avail - filled))?;
    queue!(stdout, SetForegroundColor(C_BORDER))?;
    write!(stdout, "]")?;

    queue!(stdout, ResetColor, SetAttribute(Attribute::Reset))?;
    Ok(())
}

// ── Footer ────────────────────────────────────────────────────

fn draw_footer(stdout: &mut io::Stdout, row: u16, cols: u16) -> Result<()> {
    queue!(
        stdout,
        cursor::MoveTo(0, row),
        SetBackgroundColor(C_PANEL),
        SetForegroundColor(C_DIM),
    )?;
    let keys = "  [Q] Quit    [Up/Down] Navigate projects    [Enter] Toggle snapshot tree";
    let padded = format!("{:<width$}", &keys[..keys.len().min(cols as usize)], width = cols as usize);
    write!(stdout, "{}", padded)?;
    queue!(stdout, ResetColor)?;
    Ok(())
}

// ── Project discovery ─────────────────────────────────────────

fn discover_projects() -> Result<Vec<ProjectInfo>> {
    let mut projects = Vec::new();

    // Scan common project roots: home dir, current dir
    let search_roots: Vec<PathBuf> = [
        std::env::current_dir().ok(),
        dirs_home(),
    ]
    .into_iter()
    .flatten()
    .collect();

    for root in &search_roots {
        scan_for_projects(root, 0, 4, &mut projects);
    }

    // Deduplicate by canonical path
    projects.sort_by(|a, b| a.path.cmp(&b.path));
    projects.dedup_by(|a, b| a.path == b.path);

    Ok(projects)
}

fn scan_for_projects(dir: &PathBuf, depth: u32, max_depth: u32, out: &mut Vec<ProjectInfo>) {
    if depth > max_depth { return; }

    let ilocker = dir.join(".ilocker");
    if ilocker.is_dir() {
        if let Ok(proj) = load_project_info(dir, &ilocker) {
            out.push(proj);
            return;  // Don't recurse inside an ilocker project
        }
    }

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                // Skip hidden dirs (except .ilocker handled above) and dep dirs
                if name.starts_with('.') || name == "node_modules" || name == "target" {
                    continue;
                }
                scan_for_projects(&path, depth + 1, max_depth, out);
            }
        }
    }
}

fn load_project_info(project_root: &PathBuf, ilocker_dir: &PathBuf) -> Result<ProjectInfo> {
    let config_raw = std::fs::read_to_string(ilocker_dir.join("config.json"))?;
    let config: serde_json::Value = serde_json::from_str(&config_raw)?;

    let project_key = config["key"].as_str().unwrap_or("unknown").to_string();

    let db_file = db_path(ilocker_dir);
    let conn    = db::open(&db_file)?;
    let snaps   = db::list_snapshots(&conn)?;

    // Logical bytes: sum of total_bytes across all snapshots
    let logical_bytes: u64 = snaps.iter().map(|s| s.total_bytes as u64).sum();

    // Actual disk usage of .ilocker/
    let actual_bytes = dir_size(ilocker_dir);

    let name = project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| project_root.display().to_string());

    Ok(ProjectInfo {
        path: project_root.clone(),
        name,
        project_key,
        snapshot_count: snaps.len(),
        snapshots: snaps,
        logical_bytes,
        actual_bytes,
    })
}

fn dir_size(path: &PathBuf) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
        .sum()
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

// ── Helpers ───────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Ne jamais couper au milieu d'un caractère UTF-8 multi-octets — un message
    // de snapshot accentué (courant ici, l'outil est francophone) pouvait faire
    // paniquer le dashboard si la coupe tombait en plein milieu d'un caractère
    // comme 'é'. On recule jusqu'à la frontière de caractère valide la plus proche.
    let safe_boundary = |limit: usize| -> usize {
        let mut end = limit.min(s.len());
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        end
    };
    if max > 3 {
        let end = safe_boundary(max - 3);
        format!("{}...", &s[..end])
    } else {
        let end = safe_boundary(max);
        s[..end].to_string()
    }
}

/// Detect if the terminal supports 24-bit true color.
/// Checks COLORTERM env var (set by iTerm2, kitty, alacritty, Windows Terminal…)
fn supports_true_color() -> bool {
    match std::env::var("COLORTERM").as_deref() {
        Ok("truecolor") | Ok("24bit") => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string_is_unchanged() {
        assert_eq!(truncate("court", 30), "court");
    }

    #[test]
    fn truncate_ascii_matches_previous_behavior() {
        // Comportement inchangé pour de l'ASCII pur : chaque offset d'octet
        // est une frontière de caractère valide, donc identique à l'ancienne
        // implémentation naïve.
        let s = "a".repeat(50);
        let out = truncate(&s, 20);
        assert_eq!(out, format!("{}...", "a".repeat(17)));
        assert_eq!(out.len(), 20);
    }

    #[test]
    fn truncate_never_exceeds_max_bytes() {
        let s = "a".repeat(50);
        for max in 0..s.len() + 5 {
            assert!(truncate(&s, max).len() <= max);
        }
    }

    #[test]
    fn truncate_exact_case_that_panicked_before_the_fix() {
        // Message de snapshot réaliste et francophone — panique reproduite lors de
        // l'audit avec max=30 (coupe en plein milieu du 'é' de "réseau").
        let msg = "fix: gestion des erreurs r\u{e9}seau pendant le t\u{e9}l\u{e9}chargement des chunks Hyperscale";
        let out = truncate(msg, 30);
        assert!(out.ends_with("..."));
        assert!(out.len() <= 30);
    }

    #[test]
    fn truncate_never_panics_across_all_cut_points_of_a_multibyte_string() {
        // Teste EXHAUSTIVEMENT chaque position de coupe possible autour d'un
        // caractère multi-octets ('é' = 2 octets) pour bannir toute régression.
        let msg = "fix: gestion des erreurs r\u{e9}seau pendant le t\u{e9}l\u{e9}chargement des chunks Hyperscale";
        for max in 0..=msg.len() + 3 {
            let out = truncate(msg, max);
            assert!(out.len() <= max);
            assert!(out.chars().count() <= out.len()); // UTF-8 valide par construction de String
        }
    }

    #[test]
    fn truncate_handles_multibyte_char_at_every_position() {
        let s = "12345\u{e9}67890"; // 'é' aux octets 5..7
        for max in 0..=s.len() {
            let _ = truncate(s, max); // ne doit jamais paniquer
        }
    }

    #[test]
    fn truncate_max_zero_returns_empty() {
        assert_eq!(truncate("bonjour", 0), "");
    }
}
