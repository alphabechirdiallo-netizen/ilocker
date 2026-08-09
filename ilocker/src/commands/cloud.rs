// ============================================================
//  commands/cloud.rs — BYOC : connecte le cloud de l'UTILISATEUR
//
//  Principe : ilocker ne stocke RIEN. L'utilisateur connecte son
//  propre AWS S3 / Google Cloud Storage / DigitalOcean Spaces /
//  Supabase Storage / Backblaze B2 / Cloudflare R2 / Wasabi /
//  MinIO auto-hébergé. Tout est chiffré de bout en bout
//  (ChaCha20-Poly1305, clé dérivée de la clé du projet) avant de
//  quitter la machine — le bucket ne contient jamais que du
//  chiffré, quel que soit le provider choisi.
//
//  Aucune authentification auprès d'un serveur ilocker n'est
//  requise pour quoi que ce soit ci-dessous : 100% autonome.
//
//  iloc config cloud add|list|use|remove   — gérer des profils
//  iloc push  [--file ...] [--profile ...]  — envoyer vers le cloud
//  iloc pull  [--file ...] [--profile ...]  — restaurer depuis le cloud
//  iloc cloud usage  [--profile ...]        — espace utilisé
//  iloc cloud gc     [--profile ...]        — nettoyer les chunks orphelins
//  iloc cloud doctor [--profile ...]        — tester la connexion
//  iloc cloud verify [--profile ...]        — vérifier l'intégrité distante
// ============================================================

use crate::chunker::{self, chunk_dir, FileManifest, SnapshotManifest};
use crate::cloud_crypto::CloudCrypto;
use crate::cloud_store::{self, CloudCredentials, CloudProfile, CloudProvider};
use crate::commands::undo::is_in_preserved_dir;
use crate::db;
use crate::cloud_backend::CloudBackend;
use crate::utils::{db_path, human_bytes};
use anyhow::{bail, Context, Result};
use chrono::Utc;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

pub fn manifest_key(snapshot_id: &str) -> String {
    format!("manifests/{}.enc", snapshot_id)
}

// ── iloc config cloud add ───────────────────────────────────────

pub async fn run_config_cloud_add(name_opt: Option<String>, activate: bool) -> Result<()> {
    println!();
    println!("{}", "ilocker — Connecter votre propre cloud".bold());
    println!(
        "  {}",
        "Vos identifiants sont stockés dans le trousseau du système (Keychain / Credential Manager / kernel keyring) — jamais sur disque en clair.".dimmed()
    );
    println!(
        "  {}",
        "Vos données sont chiffrées avant de quitter votre machine : votre bucket ne contiendra jamais que du chiffré.".dimmed()
    );
    println!();

    // ── 1. Nom du profil ───────────────────────────────────────
    let existing = cloud_store::list_profiles()?;
    let default_name = if existing.profiles.is_empty() { "default".to_string() } else { format!("profile-{}", existing.profiles.len() + 1) };
    let name = match name_opt {
        Some(n) => n,
        None => {
            let entered = prompt(&format!("  Nom de ce profil [{}]: ", default_name))?;
            if entered.is_empty() { default_name } else { entered }
        }
    };

    // ── 2. Provider ────────────────────────────────────────────
    println!();
    println!("  {}", "Providers disponibles :".dimmed());
    for (i, p) in CloudProvider::all().iter().enumerate() {
        println!("    {}. {}", i + 1, p.label());
    }
    println!();
    let provider_str = prompt("  Provider [s3/backblaze/minio/digitalocean/supabase/gcs/r2/wasabi/azure]: ")?;
    let provider = CloudProvider::from_str(&provider_str)
        .ok_or_else(|| anyhow::anyhow!("Provider '{}' inconnu.", provider_str))?;

    println!("  {} {}", "sélectionné:".dimmed(), provider.label().bold());
    println!("  {} {}", "astuce:".dimmed(), provider.setup_hint().dimmed());
    println!();

    // ── 3. Bucket + région ─────────────────────────────────────
    let bucket_label = match provider {
        CloudProvider::Supabase => "Nom du bucket Storage",
        CloudProvider::Azure    => "Nom du container Blob Storage",
        _                       => "Nom du bucket",
    };
    let bucket = prompt(&format!("  {}: ", bucket_label))?;
    if bucket.is_empty() { bail!("Le nom du bucket ne peut pas être vide."); }

    let region_label = if provider == CloudProvider::Azure { "Nom du compte de stockage Azure" } else { "Région" };
    let region_hint = provider.default_region_hint();
    let region = prompt(&format!("  {} [{}]: ", region_label, region_hint))?;
    let region = if region.is_empty() { region_hint.to_string() } else { region };

    // ── 4. Endpoint ─────────────────────────────────────────────
    let endpoint = if provider.requires_manual_endpoint() {
        let hint = match provider {
            CloudProvider::Minio    => "ex: http://localhost:9000 ou https://minio.mondomaine.com",
            CloudProvider::Supabase => "ex: https://<ref-projet>.supabase.co/storage/v1/s3",
            CloudProvider::R2       => "ex: https://<account-id>.r2.cloudflarestorage.com",
            _ => "",
        };
        let ep = prompt(&format!("  Endpoint ({}): ", hint))?;
        if ep.is_empty() { bail!("Ce provider nécessite un endpoint explicite."); }
        Some(ep)
    } else if provider == CloudProvider::Azure {
        // L'endpoint standard est déductible du nom de compte
        // (https://<compte>.blob.core.windows.net) — mais on laisse
        // une option pour les cas particuliers (Azure Government,
        // sovereign clouds, ou émulateur local de type Azurite).
        let default_ep = provider.endpoint_template(&region).unwrap_or_default();
        let ep = prompt(&format!("  Endpoint [{}]: ", default_ep))?;
        Some(if ep.is_empty() { default_ep } else { ep })
    } else {
        provider.endpoint_template(&region)
    };

    println!();

    // ── 5. Clés d'accès ──────────────────────────────────────────
    let (ak_label, sk_label) = match provider {
        CloudProvider::Backblaze => ("Application Key ID", "Application Key"),
        CloudProvider::Gcs       => ("Clé d'accès HMAC", "Secret HMAC"),
        CloudProvider::Azure     => ("", "Clé de compte (Access Key, key1 ou key2)"),
        _                        => ("Access Key ID", "Secret Access Key"),
    };

    // Azure Blob Storage n'a qu'UNE SEULE clé secrète (pas d'Access Key
    // ID séparé) — le nom du compte, déjà saisi ci-dessus, en tient lieu
    // d'identifiant. On ne redemande donc rien ici pour ce provider.
    let access_key = if provider == CloudProvider::Azure {
        region.clone()
    } else {
        let ak = prompt(&format!("  {}: ", ak_label))?;
        if ak.is_empty() { bail!("{} ne peut pas être vide.", ak_label); }
        ak
    };

    let secret_key = rpassword::prompt_password(format!("  {} (masqué): ", sk_label))
        .map_err(|e| anyhow::anyhow!("Lecture de la clé secrète impossible : {}", e))?;
    if secret_key.is_empty() { bail!("{} ne peut pas être vide.", sk_label); }

    println!();
    println!("{}", "  Test de connexion…".dimmed());

    // ── 6. Test de connectivité réel (PUT + GET + DELETE) ───────
    let account = format!("{}-{}", name, uuid::Uuid::new_v4());
    let creds = CloudCredentials {
        profile_name:      name.clone(),
        provider,
        bucket:            bucket.clone(),
        region:            region.clone(),
        endpoint:          endpoint.clone(),
        access_key_id:     access_key.clone(),
        secret_access_key: secret_key.clone(),
    };

    run_connectivity_test(&creds).await.context(
        "Connexion impossible — vérifiez bucket, région, endpoint et clés."
    )?;
    println!("  {} connexion vérifiée (écriture + lecture + suppression de test réussies)", "✓".green());

    // ── 7. Sauvegarde ────────────────────────────────────────────
    let profile = CloudProfile {
        name: name.clone(),
        provider,
        bucket: bucket.clone(),
        region,
        endpoint,
        account: account.clone(),
    };
    cloud_store::upsert_profile(profile, activate || existing.profiles.is_empty())?;
    cloud_store::save_secrets(&account, &access_key, &secret_key)?;

    println!();
    println!("{} Profil cloud '{}' enregistré", "✓".green().bold(), name.bold());
    println!("  {} {} · bucket {}", "provider:".dimmed(), provider.label(), bucket.cyan());
    println!(
        "  {} Vos données restent chez VOUS — ilocker n'y a jamais accès.",
        "liberté:".dimmed()
    );
    println!();
    println!("  Lancez {} pour envoyer votre projet.", "iloc push".cyan());

    Ok(())
}

pub fn run_config_cloud_list() -> Result<()> {
    let cfg = cloud_store::list_profiles()?;
    println!();
    if cfg.profiles.is_empty() {
        println!("{}", "Aucun profil cloud configuré.".yellow());
        println!("  Lancez {} pour en créer un.", "iloc config cloud add".cyan());
        println!();
        return Ok(());
    }
    println!("{}", "  Profils cloud configurés".bold());
    for p in &cfg.profiles {
        let active = cfg.active.as_deref() == Some(p.name.as_str());
        let marker = if active { "●".green() } else { "○".dimmed() };
        println!(
            "  {} {} — {} — bucket: {}",
            marker, p.name.bold(), p.provider.label(), p.bucket.cyan()
        );
    }
    println!();
    println!("{}", "  `iloc config cloud use <nom>` pour changer le profil actif.".dimmed());
    println!();
    Ok(())
}

pub fn run_config_cloud_use(name: String) -> Result<()> {
    cloud_store::set_active(&name)?;
    println!("{} profil actif : {}", "✓".green().bold(), name.bold());
    Ok(())
}

pub fn run_config_cloud_remove(name: String) -> Result<()> {
    if cloud_store::remove_profile(&name)? {
        println!("{} profil '{}' supprimé (secrets retirés du trousseau)", "✓".green().bold(), name);
    } else {
        println!("{} aucun profil nommé '{}'", "⚠".yellow(), name);
    }
    Ok(())
}

async fn run_connectivity_test(creds: &CloudCredentials) -> Result<()> {
    let backend = CloudBackend::from_creds(creds)?;
    let key = "_ilocker_healthcheck/probe.txt";
    let payload = format!("ilocker connectivity test — {}", Utc::now().to_rfc3339());

    backend.put_raw(key, payload.as_bytes()).await.context("PUT a échoué (permissions ? bucket/container inexistant ?)")?;
    let back = backend.get_raw(key).await.context("GET a échoué après écriture réussie — étrange")?;
    if back != payload.as_bytes() {
        bail!("Les données relues ne correspondent pas à celles écrites.");
    }
    let _ = backend.delete_raw(key).await;
    Ok(())
}

// ── iloc cloud doctor ────────────────────────────────────────────

pub async fn run_doctor(profile: Option<String>) -> Result<()> {
    let creds = cloud_store::require_credentials(profile.as_deref())?;
    println!();
    println!("{} diagnostic du profil '{}'", "🩺".to_string(), creds.profile_name.bold());
    println!("  {} {}", "provider:".dimmed(), creds.provider.label());
    println!("  {} {}", "bucket:".dimmed(), creds.bucket.cyan());
    println!("  {} {}", "region:".dimmed(), creds.region);
    if let Some(ep) = &creds.endpoint {
        println!("  {} {}", "endpoint:".dimmed(), ep);
    }
    println!();
    print!("  {} écriture + lecture + suppression d'un objet de test… ", "→".cyan());
    use std::io::Write;
    std::io::stdout().flush().ok();

    match run_connectivity_test(&creds).await {
        Ok(_) => println!("{}", "OK".green().bold()),
        Err(e) => {
            println!("{}", "ÉCHEC".red().bold());
            println!();
            println!("  {} {}", "détail:".dimmed(), e);
            println!();
            println!("  {}", "Pistes courantes :".dimmed());
            println!("    • bucket inexistant ou mal nommé");
            println!("    • région/endpoint incorrect pour ce provider");
            println!("    • clé d'accès expirée ou permissions insuffisantes (PutObject/GetObject/DeleteObject)");
            return Err(e);
        }
    }
    println!();
    println!("{} ce profil est prêt pour `iloc push`", "✓".green().bold());
    Ok(())
}

// ── iloc push ─────────────────────────────────────────────────

pub async fn run_push(files: Vec<String>, profile: Option<String>) -> Result<()> {
    let creds = cloud_store::require_credentials(profile.as_deref())?;
    let s3    = CloudBackend::from_creds(&creds)?;

    let cwd          = std::env::current_dir()?;
    let ilocker_dir  = cwd.join(".ilocker");
    if !ilocker_dir.exists() {
        bail!("Not an ilocker project. Run `iloc init` first.");
    }

    let project_key = load_project_key(&ilocker_dir)?;
    let crypto      = CloudCrypto::from_project_key(&project_key);

    let db_file  = db_path(&ilocker_dir);
    let conn     = db::open(&db_file)?;
    let snap     = db::latest_snapshot(&conn)?.ok_or_else(|| {
        anyhow::anyhow!("No snapshots. Run `iloc save` first.")
    })?;

    let all_records = db::files_for_snapshot(&conn, &snap.id)?;
    let (records, not_found) = db::select_records(&all_records, &files);
    for f in &not_found {
        println!("  {} '{}' introuvable dans ce snapshot — ignoré", "⚠".yellow(), f);
    }
    let selective = !files.is_empty();
    if selective && records.is_empty() {
        bail!("Aucun des fichiers demandés n'existe dans le dernier snapshot.");
    }
    let chunk_store = chunk_dir(&ilocker_dir);
    std::fs::create_dir_all(&chunk_store)?;

    println!();
    if selective {
        println!(
            "{} Pushing {} selected file(s) from \"{}\"",
            "↑".cyan().bold(), records.len().to_string().yellow(), snap.message.bold()
        );
    } else {
        println!(
            "{} Pushing \"{}\"",
            "↑".cyan().bold(), snap.message.bold()
        );
    }
    println!(
        "  {} {} · profil: {} · bucket: {}",
        "destination:".dimmed(),
        creds.provider.label().bold(),
        creds.profile_name.cyan(),
        creds.bucket.cyan()
    );

    let t0 = Instant::now();

    // ── Chunk all files ────────────────────────────────────────
    println!();
    println!("{}", "  Chunking files…".dimmed());

    let pb = progress_bar(records.len() as u64, "chunking");
    let mut manifests: Vec<FileManifest> = Vec::new();

    for rec in &records {
        let src = crate::vault::snapshots_dir(&ilocker_dir).join(&snap.id).join(&rec.rel_path);
        let src = if src.exists() { src } else { cwd.join(&rec.rel_path) };
        if !src.exists() { pb.inc(1); continue; }
        let m = chunker::chunk_file(&src, &rec.rel_path, &chunk_store)?;
        manifests.push(m);
        pb.inc(1);
    }
    pb.finish_and_clear();

    // ── Deduplication check ─────────────────────────────────────
    println!("{}", "  Checking remote deduplication…".dimmed());

    let all_chunks: Vec<(String, String)> = manifests.iter()
        .flat_map(|m| m.chunks.iter().map(|c| (c.sha256.clone(), m.rel_path.clone())))
        .collect();
    let mut unique: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (sha, path) in all_chunks { unique.entry(sha).or_insert(path); }

    let pb = progress_bar(unique.len() as u64, "checking remote");
    let mut to_upload: Vec<(String, String)> = Vec::new();

    for (sha, path) in &unique {
        if !s3.chunk_exists(sha).await.unwrap_or(false) {
            to_upload.push((sha.clone(), path.clone()));
        }
        pb.inc(1);
    }
    pb.finish_and_clear();

    let already = unique.len() - to_upload.len();
    println!(
        "  {} {} chunks already remote · {} to upload",
        "⚡".yellow(),
        already.to_string().green(),
        to_upload.len().to_string().yellow()
    );

    // ── Encrypt + upload in parallel batches ────────────────────
    if !to_upload.is_empty() {
        let pb = progress_bar(to_upload.len() as u64, "uploading");
        let mut bytes_sent: u64 = 0;

        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
        let mut handles = Vec::new();

        for (sha, _rel) in to_upload.clone() {
            let chunk_store2 = chunk_store.clone();
            let s3_provider  = creds.provider;
            let s3_bucket    = creds.bucket.clone();
            let s3_region    = creds.region.clone();
            let s3_endpoint  = creds.endpoint.clone();
            let s3_ak        = creds.access_key_id.clone();
            let s3_sk        = creds.secret_access_key.clone();
            let crypto2      = CloudCrypto::from_project_key(&project_key);
            let permit       = sem.clone().acquire_owned().await?;

            let handle = tokio::spawn(async move {
                let _permit = permit;
                let raw = chunker::load_chunk(&chunk_store2, &sha)?;
                let enc = crypto2.encrypt(&raw, &sha)?;
                let client = CloudBackend::new(
                    s3_provider, &s3_bucket, &s3_region,
                    s3_endpoint.as_deref(),
                    &s3_ak, &s3_sk,
                )?;
                client.put_chunk(&sha, &enc).await?;
                Ok::<u64, anyhow::Error>(enc.len() as u64)
            });
            handles.push(handle);
        }

        for handle in handles {
            match handle.await {
                Ok(Ok(n))  => { bytes_sent += n; pb.inc(1); }
                Ok(Err(e)) => { pb.inc(1); eprintln!("  upload error: {}", e); }
                Err(e)     => { pb.inc(1); eprintln!("  task error: {}", e); }
            }
        }
        pb.finish_and_clear();
        println!(
            "  {} {} uploaded (encrypted, deduplicated)",
            "✓".green(),
            human_bytes(bytes_sent)
        );
    }

    // ── Upload manifest (clé propre : manifests/<id>.enc) ──────
    let snapshot_manifest = SnapshotManifest {
        snapshot_id: snap.id.clone(),
        project_key: project_key.clone(),
        files:       manifests,
        created_at:  Utc::now().to_rfc3339(),
        expires_at:  None,
    };
    let manifest_json = serde_json::to_vec(&snapshot_manifest)?;
    let manifest_enc  = crypto.encrypt(&manifest_json, &snap.id)?;
    s3.put_raw(&manifest_key(&snap.id), &manifest_enc).await
        .context("Échec de l'upload du manifest")?;

    let elapsed = t0.elapsed();
    println!();
    println!(
        "{} Push complete in {:.1}s",
        "✓".green().bold(), elapsed.as_secs_f64()
    );
    println!(
        "  {}",
        "All data is encrypted (ChaCha20-Poly1305) — the bucket contains only ciphertext.".dimmed()
    );
    println!(
        "  {} restaurable n'importe où avec : {} (clé projet requise)",
        "↪".dimmed(),
        format!("iloc pull --profile {}", creds.profile_name).cyan()
    );

    Ok(())
}

// ── iloc pull ─────────────────────────────────────────────────

pub async fn run_pull(
    snap_id: Option<String>,
    dest:    Option<PathBuf>,
    files:   Vec<String>,
    profile: Option<String>,
) -> Result<()> {
    let creds = cloud_store::require_credentials(profile.as_deref())?;

    println!();
    println!("{} Pulling from {} (profil: {})", "↓".cyan().bold(), creds.provider.label().bold(), creds.profile_name.cyan());

    let cwd         = std::env::current_dir()?;
    let ilocker_dir = cwd.join(".ilocker");
    if !ilocker_dir.exists() {
        bail!("Not an ilocker project. Run `iloc init` first (or `iloc clone` if you don't have it locally yet).");
    }

    let project_key = load_project_key(&ilocker_dir)?;
    let crypto      = CloudCrypto::from_project_key(&project_key);
    let s3          = CloudBackend::from_creds(&creds)?;

    let db_file = db_path(&ilocker_dir);
    let conn    = db::open(&db_file)?;

    let snap = match &snap_id {
        Some(id) => db::resolve_snapshot_ref(&conn, id)?
            .ok_or_else(|| anyhow::anyhow!("Snapshot '{}' not found locally", id))?,
        None     => db::latest_snapshot(&conn)?
            .ok_or_else(|| anyhow::anyhow!("No snapshots found locally. Run `iloc save` then `iloc push` first."))?,
    };

    println!("  {} \"{}\"", "target:".dimmed(), snap.message.bold());

    let manifest_enc = s3.get_raw(&manifest_key(&snap.id)).await
        .map_err(|e| anyhow::anyhow!(
            "Cannot fetch manifest for this snapshot. Was it pushed? ({})", e
        ))?;

    let manifest_json = crypto.decrypt(&manifest_enc)?;
    let mut manifest: SnapshotManifest = serde_json::from_slice(&manifest_json)
        .map_err(|e| anyhow::anyhow!("Manifest decryption/parse error: {}", e))?;

    // ── Filtrage sélectif (--file) ───────────────────────────────
    let selective = !files.is_empty();
    if selective {
        let normalized: Vec<String> = files.iter()
            .map(|f| f.replace('\\', "/").trim_start_matches("./").trim_start_matches('/').to_string())
            .collect();
        let before = manifest.files.len();
        manifest.files.retain(|f| normalized.iter().any(|w| &f.rel_path == w));
        let missing: Vec<&String> = normalized.iter()
            .filter(|w| !manifest.files.iter().any(|f| &f.rel_path == *w))
            .collect();
        for m in &missing {
            println!("  {} '{}' introuvable dans ce manifest — ignoré", "⚠".yellow(), m);
        }
        if manifest.files.is_empty() {
            bail!("Aucun des fichiers demandés n'existe dans ce snapshot distant.");
        }
        let _ = before;
    }

    let total_chunks: usize = manifest.files.iter().map(|f| f.chunks.len()).sum();
    println!(
        "  {} {} files · {} chunks",
        "manifest:".dimmed(),
        manifest.files.len().to_string().yellow(),
        total_chunks.to_string().yellow()
    );

    // ── Téléchargement des chunks manquants localement ──────────
    let chunk_store = chunk_dir(&ilocker_dir);
    std::fs::create_dir_all(&chunk_store)?;

    let missing: Vec<String> = manifest.files.iter()
        .flat_map(|f| chunker::missing_chunks(f, &chunk_store).into_iter().map(|c| c.sha256.clone()))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let already = total_chunks.saturating_sub(missing.len());
    println!(
        "  {} {} chunks local · {} to download",
        "⚡".yellow(),
        already.to_string().green(),
        missing.len().to_string().yellow()
    );

    if !missing.is_empty() {
        let pb  = progress_bar(missing.len() as u64, "downloading");
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
        let mut handles = Vec::new();

        for sha in missing.clone() {
            let s3_provider  = creds.provider;
            let s3_bucket    = creds.bucket.clone();
            let s3_region    = creds.region.clone();
            let s3_endpoint  = creds.endpoint.clone();
            let s3_ak        = creds.access_key_id.clone();
            let s3_sk        = creds.secret_access_key.clone();
            let crypto2      = CloudCrypto::from_project_key(&project_key);
            let chunk_store2 = chunk_store.clone();
            let permit       = sem.clone().acquire_owned().await?;

            let handle = tokio::spawn(async move {
                let _permit = permit;
                let client = CloudBackend::new(
                    s3_provider, &s3_bucket, &s3_region,
                    s3_endpoint.as_deref(),
                    &s3_ak, &s3_sk,
                )?;
                let enc   = client.get_chunk(&sha).await?;
                let plain = crypto2.decrypt(&enc)?;

                let actual = hex::encode(Sha256::digest(&plain));
                if actual != sha {
                    anyhow::bail!("Chunk integrity failure: expected {} got {}", sha, actual);
                }

                let prefix = &sha[..2];
                let dir    = chunk_store2.join(prefix);
                std::fs::create_dir_all(&dir)?;
                std::fs::write(dir.join(&sha), &plain)?;
                Ok::<(), anyhow::Error>(())
            });
            handles.push(handle);
        }

        for handle in handles {
            match handle.await {
                Ok(Ok(_))  => pb.inc(1),
                Ok(Err(e)) => { pb.inc(1); eprintln!("  download error: {}", e); }
                Err(e)     => { pb.inc(1); eprintln!("  task error: {}", e); }
            }
        }
        pb.finish_and_clear();
    }

    // ── Filet de sécurité : snapshot local avant tout écrasement ──
    // (sauf si la destination est un dossier vide / différent du
    // projet courant — alors rien d'existant à protéger)
    let dest_dir = dest.clone().unwrap_or_else(|| cwd.clone());
    let restoring_in_place = dest_dir == cwd;
    if restoring_in_place {
        println!("{}", "  creating safety snapshot before applying pull…".dimmed());
        crate::commands::save::run(&format!(
            "(pre-pull safety) before restoring snapshot from {}", creds.provider.label()
        )).context(
            "Le snapshot de sécurité avant restauration a échoué — \
             arrêt par précaution, rien n'a été écrasé. Résolvez le problème \
             ci-dessus (espace disque, permissions…) puis relancez `iloc pull`."
        )?;
    }

    // ── Réassemblage en zone tampon, puis application sûre ──────
    let staging = ilocker_dir.join(".pull-staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;

    println!("{}", "  Reassembling files…".dimmed());
    let pb = progress_bar(manifest.files.len() as u64, "reassembling");
    for fm in &manifest.files {
        let staged_path = staging.join(&fm.rel_path);
        chunker::reassemble_file(fm, &chunk_store, &staged_path)?;
        pb.inc(1);
    }
    pb.finish_and_clear();

    let mut applied = 0usize;
    let mut preserved = 0usize;
    for fm in &manifest.files {
        if is_in_preserved_dir(&fm.rel_path) {
            preserved += 1;
            continue;
        }
        let staged_path = staging.join(&fm.rel_path);
        let final_path  = dest_dir.join(&fm.rel_path);
        if !staged_path.exists() { continue; }
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if final_path.exists() {
            std::fs::remove_file(&final_path).or_else(|_| std::fs::remove_dir_all(&final_path)).ok();
        }
        std::fs::rename(&staged_path, &final_path)
            .or_else(|_| std::fs::copy(&staged_path, &final_path).map(|_| ()))?;
        applied += 1;
    }
    let _ = std::fs::remove_dir_all(&staging);

    println!();
    println!("{} Pull complete", "✓".green().bold());
    println!(
        "  {} {} files restored · {} dep-dir files preserved · destination: {}",
        "●".green(),
        applied.to_string().green(),
        preserved.to_string().yellow(),
        dest_dir.display()
    );
    println!(
        "  {} les fichiers absents du manifest n'ont pas été touchés (pull additif, jamais destructeur)",
        "ℹ".cyan().dimmed()
    );

    Ok(())
}

// ── iloc cloud usage ──────────────────────────────────────────

pub async fn run_usage(profile: Option<String>) -> Result<()> {
    let creds = cloud_store::require_credentials(profile.as_deref())?;
    let s3    = CloudBackend::from_creds(&creds)?;

    println!();
    println!("{} espace utilisé — profil '{}'", "📦".to_string(), creds.profile_name.bold());
    println!("{}", "  Listing des objets distants…".dimmed());

    let chunks    = s3.list_all("chunks/").await.context("Échec du listing des chunks")?;
    let manifests = s3.list_all("manifests/").await.context("Échec du listing des manifests")?;

    let chunk_bytes: u64    = chunks.iter().map(|(_, s)| s).sum();
    let manifest_bytes: u64 = manifests.iter().map(|(_, s)| s).sum();
    let share_manifest_count = manifests.iter().filter(|(k, _)| k.contains("-share-")).count();

    println!();
    println!("  {} {} ({} objets)", "chunks:".dimmed(), human_bytes(chunk_bytes).green(), chunks.len());
    println!(
        "  {} {} ({} objets, dont {} lien(s) de partage sélectif)",
        "manifests:".dimmed(), human_bytes(manifest_bytes).green(), manifests.len(), share_manifest_count
    );
    println!("  {} {}", "total:".dimmed(), human_bytes(chunk_bytes + manifest_bytes).green().bold());
    println!();
    println!(
        "  {}",
        "Astuce : `iloc cloud gc` nettoie les chunks non référencés localement ET les liens de partage sélectif expirés.".dimmed()
    );

    Ok(())
}

// ── iloc cloud verify ─────────────────────────────────────────

pub async fn run_verify(profile: Option<String>) -> Result<()> {
    let creds = cloud_store::require_credentials(profile.as_deref())?;
    let s3    = CloudBackend::from_creds(&creds)?;

    let cwd         = std::env::current_dir()?;
    let ilocker_dir = cwd.join(".ilocker");
    if !ilocker_dir.exists() { bail!("Not an ilocker project. Run `iloc init` first."); }

    let project_key = load_project_key(&ilocker_dir)?;
    let crypto       = CloudCrypto::from_project_key(&project_key);

    let db_file = db_path(&ilocker_dir);
    let conn    = db::open(&db_file)?;
    let snaps   = db::list_snapshots(&conn)?;

    if snaps.is_empty() {
        println!("{}", "Aucun snapshot local à vérifier.".dimmed());
        return Ok(());
    }

    println!();
    println!("{} vérification distante — profil '{}'", "→".cyan(), creds.profile_name.bold());

    let mut total_pushed = 0usize;
    let mut total_missing_chunks = 0usize;

    for snap in &snaps {
        let key = manifest_key(&snap.id);
        let manifest_enc = match s3.get_raw(&key).await {
            Ok(b) => b,
            Err(_) => {
                println!("  {} \"{}\" — jamais pushé", "○".dimmed(), snap.message.dimmed());
                continue;
            }
        };
        total_pushed += 1;

        let manifest: SnapshotManifest = match crypto.decrypt(&manifest_enc)
            .ok()
            .and_then(|j| serde_json::from_slice(&j).ok())
        {
            Some(m) => m,
            None => {
                println!("  {} \"{}\" — manifest illisible (corruption ?)", "✗".red(), snap.message);
                continue;
            }
        };

        let mut missing = 0usize;
        for fm in &manifest.files {
            for c in &fm.chunks {
                if !s3.chunk_exists(&c.sha256).await.unwrap_or(false) {
                    missing += 1;
                }
            }
        }

        if missing == 0 {
            println!("  {} \"{}\" — intègre", "✓".green(), snap.message.bold());
        } else {
            println!("  {} \"{}\" — {} chunk(s) manquant(s) à distance", "✗".red(), snap.message, missing);
            total_missing_chunks += missing;
        }
    }

    println!();
    if total_missing_chunks == 0 {
        println!("{} {} snapshot(s) pushé(s), tous intègres", "✓".green().bold(), total_pushed);
    } else {
        println!(
            "{} {} chunk(s) manquant(s) au total — relancez `iloc push` sur les snapshots concernés",
            "⚠".yellow().bold(), total_missing_chunks
        );
    }

    Ok(())
}

// ── iloc cloud gc ──────────────────────────────────────────────

pub async fn run_gc(profile: Option<String>, yes: bool) -> Result<()> {
    let creds = cloud_store::require_credentials(profile.as_deref())?;
    let s3    = CloudBackend::from_creds(&creds)?;

    let cwd         = std::env::current_dir()?;
    let ilocker_dir = cwd.join(".ilocker");
    if !ilocker_dir.exists() { bail!("Not an ilocker project. Run `iloc init` first."); }

    println!();
    println!(
        "{}",
        "⚠ ATTENTION : cette opération supprime sur le cloud les chunks non référencés par AUCUN snapshot LOCAL,\n  ainsi que les liens de partage sélectif (`iloc share --cloud --file`) expirés.".yellow().bold()
    );
    println!(
        "{}",
        "  Si ce bucket est partagé avec une équipe et que des collègues ont des snapshots que vous n'avez pas localement, NE LANCEZ PAS ceci sans coordination — leurs données pourraient être supprimées.".yellow()
    );
    println!();

    // ── Phase 1 : chunks orphelins (comportement identique à avant) ──
    let db_file = db_path(&ilocker_dir);
    let conn    = db::open(&db_file)?;
    let snaps   = db::list_snapshots(&conn)?;

    let mut referenced: HashSet<String> = HashSet::new();
    for snap in &snaps {
        for rec in db::files_for_snapshot(&conn, &snap.id)? {
            referenced.insert(rec.sha256);
        }
    }

    println!("{}", "  Listing des chunks distants…".dimmed());
    let remote_chunks = s3.list_all("chunks/").await.context("Échec du listing des chunks")?;

    let orphan_chunks: Vec<(String, u64)> = remote_chunks.into_iter()
        .filter(|(key, _)| {
            let sha = key.rsplit('/').next().unwrap_or(key);
            !referenced.contains(sha)
        })
        .collect();
    let orphan_bytes: u64 = orphan_chunks.iter().map(|(_, s)| s).sum();

    // ── Phase 2 : manifests de partage sélectif expirés ─────────
    // Marqueur : la clé contient "-share-" (format `{snapshot_id}-share-{uuid}`,
    // voir cloud_share.rs) — les manifests de snapshot normaux (`iloc push`)
    // n'ont jamais ce motif dans leur clé et ne sont donc jamais concernés.
    println!("{}", "  Listing des manifests de partage distants…".dimmed());
    let remote_manifests = s3.list_all("manifests/").await.context("Échec du listing des manifests")?;

    let share_manifest_keys: Vec<String> = remote_manifests.into_iter()
        .map(|(key, _)| key)
        .filter(|key| key.contains("-share-"))
        .collect();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();

    let mut expired_manifests: Vec<String> = Vec::new();
    let mut skipped_undecryptable = 0usize;

    if !share_manifest_keys.is_empty() {
        // Le déchiffrement nécessite la clé du projet — best-effort :
        // un manifest illisible (autre projet dans un bucket partagé,
        // ou corruption) est simplement ignoré, jamais supprimé à l'aveugle.
        if let Ok(project_key) = load_project_key(&ilocker_dir) {
            let crypto = CloudCrypto::from_project_key(&project_key);
            for key in &share_manifest_keys {
                match s3.get_raw(key).await {
                    Ok(enc) => match crypto.decrypt(&enc) {
                        Ok(json) => match serde_json::from_slice::<SnapshotManifest>(&json) {
                            Ok(manifest) => {
                                if let Some(expires_at) = manifest.expires_at {
                                    if now > expires_at {
                                        expired_manifests.push(key.clone());
                                    }
                                }
                                // expires_at == None : manifest sans expiration
                                // définie (ne devrait pas arriver pour un
                                // -share-, mais on ne le supprime jamais par
                                // prudence si c'est le cas).
                            }
                            Err(_) => skipped_undecryptable += 1,
                        },
                        Err(_) => skipped_undecryptable += 1,
                    },
                    Err(_) => skipped_undecryptable += 1,
                }
            }
        } else {
            skipped_undecryptable += share_manifest_keys.len();
        }
    }

    if skipped_undecryptable > 0 {
        println!(
            "  {} {} manifest(s) de partage ignoré(s) (illisibles avec la clé de ce projet — probablement d'un autre projet dans ce bucket partagé)",
            "ℹ".cyan(), skipped_undecryptable
        );
    }

    // ── Rien à faire ? ───────────────────────────────────────────
    if orphan_chunks.is_empty() && expired_manifests.is_empty() {
        println!("{} aucun chunk orphelin ni lien de partage expiré — rien à nettoyer", "✓".green().bold());
        return Ok(());
    }

    println!();
    if !orphan_chunks.is_empty() {
        println!(
            "  {} {} chunk(s) orphelin(s) trouvé(s) — {} récupérables",
            "trouvé:".dimmed(), orphan_chunks.len().to_string().yellow(), human_bytes(orphan_bytes).yellow()
        );
    }
    if !expired_manifests.is_empty() {
        println!(
            "  {} {} lien(s) de partage sélectif expiré(s)",
            "trouvé:".dimmed(), expired_manifests.len().to_string().yellow()
        );
    }

    if !yes {
        let total = orphan_chunks.len() + expired_manifests.len();
        print!("  Supprimer ces {} élément(s) ? [y/N] ", total);
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut ans = String::new();
        std::io::stdin().read_line(&mut ans)?;
        if !ans.trim().eq_ignore_ascii_case("y") {
            println!("  Annulé.");
            return Ok(());
        }
    }

    let mut deleted_chunks = 0usize;
    if !orphan_chunks.is_empty() {
        let pb = progress_bar(orphan_chunks.len() as u64, "deleting chunks");
        for (key, _) in &orphan_chunks {
            if s3.delete_raw(key).await.is_ok() { deleted_chunks += 1; }
            pb.inc(1);
        }
        pb.finish_and_clear();
    }

    let mut deleted_manifests = 0usize;
    if !expired_manifests.is_empty() {
        let pb = progress_bar(expired_manifests.len() as u64, "deleting expired share links");
        for key in &expired_manifests {
            if s3.delete_raw(key).await.is_ok() { deleted_manifests += 1; }
            pb.inc(1);
        }
        pb.finish_and_clear();
    }

    println!();
    if deleted_chunks > 0 {
        println!(
            "{} {} chunk(s) supprimé(s) — {} libérés",
            "✓".green().bold(), deleted_chunks, human_bytes(orphan_bytes)
        );
    }
    if deleted_manifests > 0 {
        println!(
            "{} {} lien(s) de partage expiré(s) supprimé(s)",
            "✓".green().bold(), deleted_manifests
        );
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────

fn prompt(label: &str) -> Result<String> {
    use std::io::Write;
    print!("{}", label);
    std::io::stdout().flush()?;
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    Ok(buf.trim().to_string())
}

fn progress_bar(total: u64, msg: &'static str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template(
            "  {spinner:.cyan} [{bar:40.cyan/blue}] {pos}/{len}  {msg}"
        ).unwrap().progress_chars("█▉▊▋▌▍▎▏  ")
    );
    pb.set_message(msg);
    pb
}

pub fn load_project_key(ilocker_dir: &std::path::Path) -> Result<String> {
    let raw = std::fs::read_to_string(ilocker_dir.join("config.json"))
        .map_err(|_| anyhow::anyhow!("Cannot read .ilocker/config.json"))?;
    let v: serde_json::Value = serde_json::from_str(&raw)?;
    v["key"].as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("config.json missing 'key' field"))
}
