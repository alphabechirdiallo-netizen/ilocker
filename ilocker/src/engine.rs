// ============================================================
//  engine.rs — OS-adaptive snapshot storage engine  (v1.8.0)
//
//  Stratégie de stockage (ordre de priorité par OS) :
//
//  macOS (APFS)
//    1. clonefile(2) — CoW natif, 0 octet sur disque, aucun
//       lien physique partagé → snapshot étanche par design.
//    2. Copie physique (fallback APFS hors volume, HFS+…)
//
//  Linux (Btrfs / XFS / ZFS / ext4 ≥ 4.5)
//    1. ioctl FICLONE — CoW natif, même garantie qu'APFS.
//    2. Copie physique (fallback ext3, FAT…)
//
//  Windows (NTFS / ReFS / FAT32)
//    Hard links INTERDITS pour les snapshots :
//    VS Code (et la quasi-totalité des éditeurs Windows) font
//    des sauvegardes «in-place» qui écrasent les blocs NTFS
//    directement, corrompant silencieusement tous les hard links
//    pointant vers ces blocs — y compris ceux du snapshot.
//    → Copie physique SYSTÉMATIQUE sur Windows.
//    → ReFS CoW (via DeviceIoControl FSCTL_DUPLICATE_EXTENTS)
//       optionnel mais non implémenté ici (nécessite SE_MANAGE_VOLUME_NAME).
//
//  Immutabilité post-copie (toutes plateformes)
//    Après avoir écrit le fichier dans .ilocker/snapshots/,
//    on le rend READ-ONLY au niveau OS :
//      • Windows : SetFileAttributesW → FILE_ATTRIBUTE_READONLY
//      • POSIX   : chmod → retirage du bit d'écriture
//    Cela empêche tout écrasement accidentel et matérialise
//    clairement que le snapshot est archival.
//    La commande `iloc undo` appelle unlock_snapshot_file() avant
//    de lire le fichier, et re-verrouille après restauration.
// ============================================================

use anyhow::Result;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkMethod {
    RefLink,   // CoW clone (APFS clonefile / Linux FICLONE)
    Copy,      // copie physique (Windows NTFS, fallback)
}

// ─────────────────────────────────────────────────────────────
//  Point d'entrée principal
// ─────────────────────────────────────────────────────────────

/// Copie ou clone `src` vers `dst` en choisissant la meilleure
/// méthode disponible pour la plateforme courante.
///
/// Après écriture, `dst` est rendu READ-ONLY pour protéger
/// le snapshot contre toute modification accidentelle.
///
/// Retourne la méthode effectivement utilisée.
pub fn link_or_clone(src: &Path, dst: &Path) -> Result<LinkMethod> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let method = platform_copy(src, dst)?;
    set_readonly(dst, true)?;
    Ok(method)
}

// ─────────────────────────────────────────────────────────────
//  Dispatch par plateforme
// ─────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn platform_copy(src: &Path, dst: &Path) -> Result<LinkMethod> {
    // Priorité 1 : clonefile APFS (CoW natif)
    if try_clonefile(src, dst).is_ok() {
        return Ok(LinkMethod::RefLink);
    }
    // Fallback : copie physique
    std::fs::copy(src, dst)?;
    Ok(LinkMethod::Copy)
}

#[cfg(target_os = "linux")]
fn platform_copy(src: &Path, dst: &Path) -> Result<LinkMethod> {
    // Priorité 1 : ioctl FICLONE (Btrfs / XFS / ZFS / ext4 ≥ 4.5)
    if try_reflink_linux(src, dst).is_ok() {
        return Ok(LinkMethod::RefLink);
    }
    // Fallback : copie physique
    std::fs::copy(src, dst)?;
    Ok(LinkMethod::Copy)
}

#[cfg(target_os = "windows")]
fn platform_copy(src: &Path, dst: &Path) -> Result<LinkMethod> {
    // Sur Windows, hard links INTERDITS (corruption NTFS in-place).
    // Copie physique systématique.
    std::fs::copy(src, dst)?;
    Ok(LinkMethod::Copy)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn platform_copy(src: &Path, dst: &Path) -> Result<LinkMethod> {
    std::fs::copy(src, dst)?;
    Ok(LinkMethod::Copy)
}

// ─────────────────────────────────────────────────────────────
//  Immutabilité : verrouillage / déverrouillage READ-ONLY
// ─────────────────────────────────────────────────────────────

/// Rend `path` READ-ONLY (`readonly=true`) ou accessible en
/// écriture (`readonly=false`).
///
/// Utilisé par :
///  - `link_or_clone` pour verrouiller après écriture dans le snapshot
///  - `iloc undo`     pour déverrouiller avant lecture/restauration
pub fn set_readonly(path: &Path, readonly: bool) -> Result<()> {
    set_readonly_impl(path, readonly)
}

#[cfg(unix)]
fn set_readonly_impl(path: &Path, readonly: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path)?;
    let mut perms = meta.permissions();
    if readonly {
        // Retire tous les bits d'écriture : u=r, g=r, o=r
        let new_mode = perms.mode() & !0o222;
        perms.set_mode(new_mode);
    } else {
        // Restaure l'écriture propriétaire (u+w)
        let new_mode = perms.mode() | 0o200;
        perms.set_mode(new_mode);
    }
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(windows)]
fn set_readonly_impl(path: &Path, readonly: bool) -> Result<()> {
    use std::os::windows::fs::MetadataExt;

    // std::fs::Permissions::set_readonly() délègue à SetFileAttributesW
    // sur Windows — c'est exactement l'API que l'on veut.
    let meta = std::fs::metadata(path)?;
    let mut perms = meta.permissions();
    perms.set_readonly(readonly);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn set_readonly_impl(path: &Path, readonly: bool) -> Result<()> {
    let meta = std::fs::metadata(path)?;
    let mut perms = meta.permissions();
    perms.set_readonly(readonly);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

/// Rend TOUS les fichiers d'un répertoire de snapshot READ-ONLY.
/// Appelé en fin de `iloc save` pour sceller le snapshot entier.
pub fn seal_snapshot_dir(snap_dir: &Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(snap_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        // .deleted.json est aussi scellé
        set_readonly(entry.path(), true)?;
    }
    Ok(())
}

/// Déverrouille UN fichier de snapshot avant lecture/restauration.
/// `iloc undo` appelle cette fonction, lit le fichier, puis re-verrouille.
pub fn unlock_snapshot_file(path: &Path) -> Result<()> {
    // Ignorer si le fichier n'existe pas encore (snapshot partiel)
    if path.exists() {
        set_readonly(path, false)?;
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  macOS APFS clonefile(2)
// ─────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn try_clonefile(src: &Path, dst: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let src_c = CString::new(src.as_os_str().as_bytes())?;
    let dst_c = CString::new(dst.as_os_str().as_bytes())?;

    // clonefile(2) disponible depuis macOS 10.12 Sierra.
    // Drapeaux : 0 (comportement par défaut).
    let ret = unsafe { libc_clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0) };
    if ret == 0 {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "clonefile failed: {}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(target_os = "macos")]
extern "C" {
    #[link_name = "clonefile"]
    fn libc_clonefile(src: *const i8, dst: *const i8, flags: u32) -> i32;
}

// ─────────────────────────────────────────────────────────────
//  Linux FICLONE ioctl
// ─────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn try_reflink_linux(src: &Path, dst: &Path) -> Result<()> {
    use std::fs::{File, OpenOptions};
    use std::os::unix::io::AsRawFd;

    // FICLONE ioctl — Btrfs, XFS, ZFS, OCFS2, ext4 ≥ 4.5
    // Code ioctl : 0x40049409
    const FICLONE: u64 = 0x40049409;

    let src_file = File::open(src)?;
    let dst_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dst)?;

    let ret = unsafe { libc_ioctl(dst_file.as_raw_fd(), FICLONE, src_file.as_raw_fd()) };

    if ret == 0 {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "FICLONE ioctl failed: {}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(target_os = "linux")]
extern "C" {
    fn ioctl(fd: i32, request: u64, ...) -> i32;
}

#[cfg(target_os = "linux")]
unsafe fn libc_ioctl(fd: i32, request: u64, arg: i32) -> i32 {
    ioctl(fd, request, arg)
}

// ─────────────────────────────────────────────────────────────
//  Helpers de restauration
// ─────────────────────────────────────────────────────────────

/// Supprime un fichier de l'arbre de travail (pas du snapshot).
/// Ignore silencieusement les erreurs "not found" (idempotent).
/// Sur Windows, lève le flag READ-ONLY avant de supprimer si besoin.
pub fn remove_file_safe(path: &Path) -> Result<()> {
    // Sur Windows, un fichier READ-ONLY ne peut pas être supprimé
    // directement — on lève d'abord le verrou.
    #[cfg(windows)]
    {
        if path.exists() {
            let _ = set_readonly(path, false);
        }
    }
    match std::fs::remove_file(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Supprime récursivement un répertoire, en ignorant "not found".
#[allow(dead_code)]
pub fn remove_dir_safe(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
