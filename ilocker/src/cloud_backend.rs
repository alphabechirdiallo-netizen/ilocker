// ============================================================
//  cloud_backend.rs — Abstraction multi-protocole (S3 / Azure)
//
//  v1.14.0 — Support Azure
//  ─────────────────────────────────────────────────────────
//  Azure Blob Storage utilise un protocole HTTP totalement
//  différent de l'API S3 (auth "Shared Key" au lieu de SigV4,
//  en-têtes `x-ms-*`, format XML différent pour le listing...).
//  Plutôt que de parsemer tout le code de `if provider == Azure`,
//  ce module fournit un type `CloudBackend` qui enveloppe soit
//  un `S3Client` (AWS S3, Backblaze, MinIO, DigitalOcean,
//  Supabase, GCS, R2, Wasabi — tous compatibles S3), soit un
//  `AzureClient`, en exposant exactement la même API.
//
//  Tous les appelants existants (cloud.rs, hyperscale.rs) qui
//  utilisaient directement `S3Client` peuvent passer à
//  `CloudBackend` par un simple changement de type — le
//  comportement pour les providers S3-compatibles reste
//  STRICTEMENT IDENTIQUE (même code S3Client sous-jacent,
//  aucune régression possible pour les profils déjà configurés).
// ============================================================

use crate::azure_client::AzureClient;
use crate::cloud_store::{CloudCredentials, CloudProvider};
use crate::s3_client::S3Client;
use anyhow::Result;

pub enum CloudBackend {
    S3(S3Client),
    Azure(AzureClient),
}

impl CloudBackend {
    /// Construit le bon backend depuis des CloudCredentials résolues.
    /// Pour tous les providers S3-compatibles, ceci délègue à
    /// `S3Client::from_creds` exactement comme avant — comportement
    /// inchangé. Seul `CloudProvider::Azure` emprunte le nouveau
    /// chemin `AzureClient`.
    pub fn from_creds(creds: &CloudCredentials) -> Result<Self> {
        match creds.provider {
            CloudProvider::Azure => Ok(CloudBackend::Azure(AzureClient::from_creds(creds)?)),
            _                    => Ok(CloudBackend::S3(S3Client::from_creds(creds))),
        }
    }

    /// Construit explicitement depuis des champs bruts (utilisé dans
    /// les tâches `tokio::spawn` qui ne peuvent pas capturer
    /// `CloudCredentials` par référence). `provider` détermine le
    /// protocole ; les autres champs suivent la même convention que
    /// `CloudCredentials` (bucket/region/endpoint/access_key/secret_key).
    pub fn new(
        provider:   CloudProvider,
        bucket:     &str,
        region:     &str,
        endpoint:   Option<&str>,
        access_key: &str,
        secret_key: &str,
    ) -> Result<Self> {
        match provider {
            CloudProvider::Azure => Ok(CloudBackend::Azure(
                AzureClient::new(region, bucket, endpoint, secret_key)?
            )),
            _ => Ok(CloudBackend::S3(
                S3Client::new(bucket, region, endpoint, access_key, secret_key)
            )),
        }
    }

    pub fn chunk_key(sha256: &str) -> String {
        S3Client::chunk_key(sha256)
    }

    // ── API miroir, dispatch vers le bon backend ────────────────

    pub async fn chunk_exists(&self, sha256: &str) -> Result<bool> {
        match self {
            CloudBackend::S3(c)    => c.chunk_exists(sha256).await,
            CloudBackend::Azure(c) => c.chunk_exists(sha256).await,
        }
    }

    pub async fn put_chunk(&self, sha256: &str, data: &[u8]) -> Result<()> {
        match self {
            CloudBackend::S3(c)    => c.put_chunk(sha256, data).await,
            CloudBackend::Azure(c) => c.put_chunk(sha256, data).await,
        }
    }

    pub async fn get_chunk(&self, sha256: &str) -> Result<Vec<u8>> {
        match self {
            CloudBackend::S3(c)    => c.get_chunk(sha256).await,
            CloudBackend::Azure(c) => c.get_chunk(sha256).await,
        }
    }

    pub async fn exists(&self, key: &str) -> Result<bool> {
        match self {
            CloudBackend::S3(c)    => c.exists(key).await,
            CloudBackend::Azure(c) => c.exists(key).await,
        }
    }

    pub async fn put_raw(&self, key: &str, data: &[u8]) -> Result<()> {
        match self {
            CloudBackend::S3(c)    => c.put_raw(key, data).await,
            CloudBackend::Azure(c) => c.put_raw(key, data).await,
        }
    }

    pub async fn get_raw(&self, key: &str) -> Result<Vec<u8>> {
        match self {
            CloudBackend::S3(c)    => c.get_raw(key).await,
            CloudBackend::Azure(c) => c.get_raw(key).await,
        }
    }

    pub async fn delete_raw(&self, key: &str) -> Result<()> {
        match self {
            CloudBackend::S3(c)    => c.delete_raw(key).await,
            CloudBackend::Azure(c) => c.delete_raw(key).await,
        }
    }

    pub async fn list_all(&self, prefix: &str) -> Result<Vec<(String, u64)>> {
        match self {
            CloudBackend::S3(c)    => c.list_all(prefix).await,
            CloudBackend::Azure(c) => c.list_all(prefix).await,
        }
    }
}
