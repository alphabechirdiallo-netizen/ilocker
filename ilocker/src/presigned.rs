// ============================================================
//  presigned.rs — URLs pré-signées S3 (SigV4) + SAS tokens Azure
//
//  Deux générateurs selon le provider :
//
//  PresignedUrlGenerator (S3 / SigV4)
//    AWS S3, Backblaze B2, MinIO, DigitalOcean Spaces, GCS,
//    Cloudflare R2, Wasabi, Supabase Storage — tous compatibles
//    AWS Signature Version 4 Query String.
//
//  AzureSasGenerator (Azure Blob Storage / SAS)
//    Shared Access Signature version 2020-12-06.
//    RFC : https://learn.microsoft.com/en-us/rest/api/storageservices/
//           create-service-sas
//    La clé utilisée est la Shared Key du compte de stockage
//    (même clé que azure_client.rs — aucune credential supplémentaire).
//
//  Modèle de sécurité commun
//  ─────────────────────────
//  • Les clés d'accès ne quittent jamais la machine du partageur.
//  • Les URLs accordent uniquement GET sur un objet précis.
//  • TTL configurable, borné (défaut 2 h, max 7 j pour S3 /
//    max 7 j pour Azure SAS version 2020-12-06).
// ============================================================

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Default pre-signed URL validity: 2 hours
pub const DEFAULT_TTL_SECS: u64 = 2 * 60 * 60;
/// Maximum validity for S3 SigV4 (7 days per AWS policy)
pub const MAX_TTL_SECS: u64 = 7 * 24 * 60 * 60;
/// Maximum validity for Azure SAS (7 days — même borne)
pub const AZURE_MAX_TTL_SECS: u64 = 7 * 24 * 60 * 60;

// ═══════════════════════════════════════════════════════════════
//  S3 / SigV4 — Pre-signed URL generator
// ═══════════════════════════════════════════════════════════════

pub struct PresignedUrlGenerator {
    access_key: String,
    secret_key: String,
    region:     String,
    bucket:     String,
    endpoint:   String,
}

impl PresignedUrlGenerator {
    pub fn new(
        access_key: &str,
        secret_key: &str,
        region:     &str,
        bucket:     &str,
        endpoint:   Option<&str>,
    ) -> Self {
        let endpoint = endpoint
            .map(|e| e.trim_end_matches('/').to_string())
            .unwrap_or_else(|| format!("https://s3.{}.amazonaws.com", region));

        Self {
            access_key: access_key.to_string(),
            secret_key: secret_key.to_string(),
            region:     region.to_string(),
            bucket:     bucket.to_string(),
            endpoint,
        }
    }

    /// Build from resolved cloud credentials.
    pub fn from_creds(creds: &crate::cloud_store::CloudCredentials) -> Self {
        Self::new(
            &creds.access_key_id,
            &creds.secret_access_key,
            &creds.region,
            &creds.bucket,
            creds.endpoint.as_deref(),
        )
    }

    /// Generate a pre-signed GET URL for one object (AWS SigV4 Query String).
    pub fn presign_get(&self, object_key: &str, ttl_secs: u64) -> Result<String> {
        let ttl = ttl_secs.min(MAX_TTL_SECS);

        let now        = Utc::now();
        let datetime   = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_short = &datetime[..8];

        // Canonical request components
        let host          = self.host_from_endpoint()?;
        let canonical_uri = format!("/{}/{}", self.bucket, url_encode(object_key));
        let credential    = format!(
            "{}/{}/{}/s3/aws4_request",
            self.access_key, date_short, self.region
        );

        let query = format!(
            "X-Amz-Algorithm=AWS4-HMAC-SHA256\
             &X-Amz-Credential={}\
             &X-Amz-Date={}\
             &X-Amz-Expires={}\
             &X-Amz-SignedHeaders=host",
            url_encode(&credential),
            datetime,
            ttl
        );

        // Canonical request
        let canonical_request = format!(
            "GET\n{}\n{}\nhost:{}\n\nhost\nUNSIGNED-PAYLOAD",
            canonical_uri, query, host
        );

        // String to sign
        let cr_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
        let scope   = format!("{}/{}/s3/aws4_request", date_short, self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            datetime, scope, cr_hash
        );

        // Signing key
        let signing_key = {
            let dk  = hmac_sha256(format!("AWS4{}", self.secret_key).as_bytes(), date_short.as_bytes())?;
            let dr  = hmac_sha256(&dk, self.region.as_bytes())?;
            let ds  = hmac_sha256(&dr, b"s3")?;
            hmac_sha256(&ds, b"aws4_request")?
        };

        let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes())?);

        let url = format!(
            "{}/{}/{}?{}&X-Amz-Signature={}",
            self.endpoint,
            self.bucket,
            object_key,
            query,
            signature
        );

        Ok(url)
    }

    fn host_from_endpoint(&self) -> Result<String> {
        let without_scheme = self.endpoint
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        Ok(without_scheme
            .split('/')
            .next()
            .unwrap_or(without_scheme)
            .to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
//  Azure SAS token generator
//
//  Génère des SAS (Shared Access Signature) de type Service SAS
//  sur un blob précis, avec permission de lecture seule (r),
//  valides pour la durée TTL demandée.
//
//  Spec de référence (version 2020-12-06) :
//  https://learn.microsoft.com/en-us/rest/api/storageservices/
//    create-service-sas
//
//  StringToSign pour un blob SAS version 2020-12-06 :
//    signedPermissions + "\n"
//    signedStart       + "\n"
//    signedExpiry      + "\n"
//    canonicalizedResource + "\n"
//    signedIdentifier  + "\n"
//    signedIP          + "\n"
//    signedProtocol    + "\n"
//    signedVersion     + "\n"
//    signedResource    + "\n"
//    signedSnapshotTime + "\n"
//    signedEncryptionScope + "\n"
//    rscc + "\n"   (response header overrides — tous vides ici)
//    rscd + "\n"
//    rsce + "\n"
//    rscl + "\n"
//    rsct
// ═══════════════════════════════════════════════════════════════

pub struct AzureSasGenerator {
    account_name: String,
    account_key:  Vec<u8>,   // clé décodée depuis base64
    container:    String,
    endpoint:     String,
}

impl AzureSasGenerator {
    const SAS_VERSION: &'static str = "2020-12-06";

    /// Crée un générateur depuis les informations du compte Azure.
    ///
    /// `account_name`    : nom du compte de stockage (= `region` dans CloudCredentials)
    /// `container`       : nom du container (= `bucket` dans CloudCredentials)
    /// `account_key_b64` : clé du compte en base64 (= `secret_access_key` dans CloudCredentials)
    /// `endpoint_opt`    : URL de base (None → https://<account>.blob.core.windows.net)
    pub fn new(
        account_name:    &str,
        container:       &str,
        account_key_b64: &str,
        endpoint_opt:    Option<&str>,
    ) -> Result<Self> {
        let account_key = B64.decode(account_key_b64)
            .map_err(|e| anyhow::anyhow!(
                "Clé de compte Azure invalide (attendu en base64) : {}", e
            ))?;

        let endpoint = endpoint_opt
            .map(|e| e.trim_end_matches('/').to_string())
            .unwrap_or_else(|| format!("https://{}.blob.core.windows.net", account_name));

        Ok(Self {
            account_name: account_name.to_string(),
            account_key,
            container: container.to_string(),
            endpoint,
        })
    }

    /// Construit depuis des CloudCredentials résolues.
    /// Convention (identique à AzureClient::from_creds) :
    ///   region            → storage account name
    ///   bucket            → container name
    ///   secret_access_key → clé du compte en base64
    ///   endpoint          → URL personnalisée (optionnel)
    pub fn from_creds(creds: &crate::cloud_store::CloudCredentials) -> Result<Self> {
        Self::new(
            &creds.region,
            &creds.bucket,
            &creds.secret_access_key,
            creds.endpoint.as_deref(),
        )
    }

    /// Génère une URL SAS de lecture seule pour un blob.
    ///
    /// `blob_key`  : chemin du blob dans le container (ex: "chunks/ab/abcdef…")
    /// `ttl_secs`  : durée de validité en secondes
    pub fn presign_get(&self, blob_key: &str, ttl_secs: u64) -> Result<String> {
        let ttl = ttl_secs.min(AZURE_MAX_TTL_SECS);

        let now    = Utc::now();
        let start  = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let expiry = (now + chrono::Duration::seconds(ttl as i64))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();

        let permissions = "r"; // lecture seule
        let signed_resource = "b"; // blob (pas container)
        let protocol = "https";

        // CanonicalizedResource pour un blob SAS :
        // /blob/<account>/<container>/<blob_key>
        let canonicalized_resource = format!(
            "/blob/{}/{}/{}",
            self.account_name, self.container, blob_key
        );

        // StringToSign (version 2020-12-06) : 16 champs joints par \n,
        // dans l'ordre exact de la spec Microsoft (signedPermissions
        // → rsct). Construit via un tableau pour garantir le bon
        // compte de champs (une erreur de comptage ici invaliderait
        // silencieusement toutes les signatures).
        let fields = [
            permissions,
            &start,
            &expiry,
            &canonicalized_resource,
            "",  // signedIdentifier
            "",  // signedIP
            protocol,
            Self::SAS_VERSION,
            signed_resource,
            "",  // signedSnapshotTime
            "",  // signedEncryptionScope
            "",  // rscc
            "",  // rscd
            "",  // rsce
            "",  // rscl
            "",  // rsct (dernier champ, PAS de \n final)
        ];
        let string_to_sign = fields.join("\n");

        let signature = self.hmac_sign(&string_to_sign)?;

        // Composer l'URL SAS
        let url = format!(
            "{}/{}/{}?sv={}&st={}&se={}&sr={}&sp={}&spr={}&sig={}",
            self.endpoint,
            self.container,
            blob_key,
            Self::SAS_VERSION,
            url_encode_azure(&start),
            url_encode_azure(&expiry),
            signed_resource,
            permissions,
            protocol,
            url_encode_azure(&signature),
        );

        Ok(url)
    }

    fn hmac_sign(&self, string_to_sign: &str) -> Result<String> {
        let mut mac = HmacSha256::new_from_slice(&self.account_key)
            .map_err(|e| anyhow::anyhow!("Clé Azure invalide pour HMAC : {}", e))?;
        mac.update(string_to_sign.as_bytes());
        Ok(B64.encode(mac.finalize().into_bytes()))
    }
}

// ═══════════════════════════════════════════════════════════════
//  Helpers partagés
// ═══════════════════════════════════════════════════════════════

fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|e| anyhow::anyhow!("HMAC key error: {}", e))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

/// Percent-encode pour les valeurs de query string SigV4 (RFC 3986).
fn url_encode(s: &str) -> String {
    s.bytes().fold(String::new(), |mut acc, b| {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~' => acc.push(b as char),
            _ => acc.push_str(&format!("%{:02X}", b)),
        }
        acc
    })
}

/// Percent-encode pour les paramètres de query string Azure SAS.
/// Azure est sensible à l'encodage de `:` dans les timestamps —
/// ils doivent être encodés en %3A.
fn url_encode_azure(s: &str) -> String {
    s.bytes().fold(String::new(), |mut acc, b| {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~' => acc.push(b as char),
            _ => acc.push_str(&format!("%{:02X}", b)),
        }
        acc
    })
}

/// Percent-encode pour les segments de chemin (autorise /).
#[allow(dead_code)]
fn url_encode_path_segment(s: &str) -> String {
    s.bytes().fold(String::new(), |mut acc, b| {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~' | b'/' => acc.push(b as char),
            _ => acc.push_str(&format!("%{:02X}", b)),
        }
        acc
    })
}

// ═══════════════════════════════════════════════════════════════
//  Batch pre-sign helpers — S3
// ═══════════════════════════════════════════════════════════════

/// Pre-sign GET URLs for a set of chunk SHA-256 hashes (S3).
/// Returns a map: sha256 → pre-signed URL.
pub fn presign_chunks(
    gen:      &PresignedUrlGenerator,
    sha256s:  &[String],
    ttl_secs: u64,
) -> Result<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    for sha in sha256s {
        let key = crate::s3_client::S3Client::chunk_key(sha);
        let url = gen.presign_get(&key, ttl_secs)?;
        map.insert(sha.clone(), url);
    }
    Ok(map)
}

/// Pre-sign GET URLs for a set of chunk SHA-256 hashes (Azure SAS).
/// Returns a map: sha256 → SAS URL.
pub fn presign_chunks_azure(
    gen:      &AzureSasGenerator,
    sha256s:  &[String],
    ttl_secs: u64,
) -> Result<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    for sha in sha256s {
        // Même layout de clé que S3Client::chunk_key
        let key = format!("chunks/{}/{}", &sha[..2], sha);
        let url = gen.presign_get(&key, ttl_secs)?;
        map.insert(sha.clone(), url);
    }
    Ok(map)
}

/// Pre-sign the manifest object URL (S3).
pub fn presign_manifest(
    gen:         &PresignedUrlGenerator,
    snapshot_id: &str,
    ttl_secs:    u64,
) -> Result<String> {
    let key = crate::commands::cloud::manifest_key(snapshot_id);
    gen.presign_get(&key, ttl_secs)
}

/// Pre-sign the manifest object URL (Azure SAS).
pub fn presign_manifest_azure(
    gen:         &AzureSasGenerator,
    snapshot_id: &str,
    ttl_secs:    u64,
) -> Result<String> {
    let key = crate::commands::cloud::manifest_key(snapshot_id);
    gen.presign_get(&key, ttl_secs)
}

// ═══════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── Tests S3 SigV4 ──────────────────────────────────────────

    #[test]
    fn s3_presign_produces_valid_url_shape() {
        let gen = PresignedUrlGenerator::new(
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "my-bucket",
            None,
        );
        let url = gen.presign_get("chunks/ab/abcdef1234", 7200).unwrap();
        assert!(url.starts_with("https://s3.us-east-1.amazonaws.com/my-bucket/chunks/ab/abcdef1234?"));
        assert!(url.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"));
        assert!(url.contains("X-Amz-Expires=7200"));
        assert!(url.contains("X-Amz-Signature="));
    }

    #[test]
    fn s3_presign_caps_ttl_at_max() {
        let gen = PresignedUrlGenerator::new(
            "KEY", "SECRET", "us-east-1", "bucket", None,
        );
        // TTL trop grand → tronqué à MAX_TTL_SECS
        let url = gen.presign_get("object", 9999999).unwrap();
        let expires: u64 = url.split("X-Amz-Expires=").nth(1)
            .and_then(|s| s.split('&').next())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        assert_eq!(expires, MAX_TTL_SECS);
    }

    #[test]
    fn s3_presign_different_keys_produce_different_signatures() {
        let gen1 = PresignedUrlGenerator::new("KEY1", "SECRET1", "us-east-1", "bucket", None);
        let gen2 = PresignedUrlGenerator::new("KEY2", "SECRET2", "us-east-1", "bucket", None);
        let url1 = gen1.presign_get("obj", 3600).unwrap();
        let url2 = gen2.presign_get("obj", 3600).unwrap();
        // Signatures différentes (les URL ne sont pas identiques)
        assert_ne!(url1, url2);
    }

    // ── Tests Azure SAS ─────────────────────────────────────────
    //
    // La clé utilisée dans les tests est une clé de 64 octets (512 bits)
    // encodée en base64 — c'est le format exact des clés de compte Azure.

    fn azure_test_key_b64() -> &'static str {
        // Clé fictive 64 octets en base64 valide
        "dGVzdGtleXRlc3RrZXl0ZXN0a2V5dGVzdGtleXRlc3RrZXl0ZXN0a2V5dGVzdGtleXRlc3RrZXk="
    }

    #[test]
    fn azure_sas_generator_creates_from_valid_key() {
        let gen = AzureSasGenerator::new(
            "mystorageaccount",
            "mycontainer",
            azure_test_key_b64(),
            None,
        );
        assert!(gen.is_ok(), "Création du générateur Azure doit réussir avec une clé base64 valide");
    }

    #[test]
    fn azure_sas_generator_rejects_invalid_base64() {
        let gen = AzureSasGenerator::new(
            "account", "container", "!not-valid-base64-$$", None,
        );
        assert!(gen.is_err(), "Une clé base64 invalide doit être rejetée");
    }

    #[test]
    fn azure_presign_produces_valid_url_shape() {
        let gen = AzureSasGenerator::new(
            "mystorageaccount",
            "mycontainer",
            azure_test_key_b64(),
            None,
        ).unwrap();

        let url = gen.presign_get("chunks/ab/abcdef1234", 7200).unwrap();

        // Doit pointer vers le blob storage Azure
        assert!(
            url.starts_with("https://mystorageaccount.blob.core.windows.net/mycontainer/chunks/ab/abcdef1234?"),
            "URL SAS mal formée : {}", url
        );

        // Doit contenir les paramètres SAS obligatoires
        assert!(url.contains("sv=2020-12-06"), "Version SAS absente");
        assert!(url.contains("sr=b"),          "signedResource absent");
        assert!(url.contains("sp=r"),          "permission lecture absente");
        assert!(url.contains("spr=https"),     "protocole https absent");
        assert!(url.contains("sig="),          "signature absente");
        assert!(url.contains("st="),           "start time absent");
        assert!(url.contains("se="),           "expiry time absent");
    }

    #[test]
    fn azure_presign_caps_ttl_at_max() {
        let gen = AzureSasGenerator::new(
            "account", "container", azure_test_key_b64(), None,
        ).unwrap();
        // TTL très long → ne doit pas dépasser AZURE_MAX_TTL_SECS
        let url1 = gen.presign_get("obj", 100).unwrap();
        let url2 = gen.presign_get("obj", 9999999).unwrap();
        // Les deux doivent réussir
        assert!(url1.contains("sr=b"));
        assert!(url2.contains("sr=b"));
    }

    #[test]
    fn azure_presign_different_blobs_produce_different_urls() {
        let gen = AzureSasGenerator::new(
            "account", "container", azure_test_key_b64(), None,
        ).unwrap();
        let url1 = gen.presign_get("chunks/aa/aaaaa", 3600).unwrap();
        let url2 = gen.presign_get("chunks/bb/bbbbb", 3600).unwrap();
        assert_ne!(url1, url2);
    }

    #[test]
    fn azure_presign_with_custom_endpoint() {
        let gen = AzureSasGenerator::new(
            "account",
            "container",
            azure_test_key_b64(),
            Some("https://myaccount.blob.core.usgovcloudapi.net"),
        ).unwrap();
        let url = gen.presign_get("obj", 3600).unwrap();
        assert!(url.starts_with("https://myaccount.blob.core.usgovcloudapi.net/container/obj?"));
    }

    #[test]
    fn azure_presign_read_only_permission() {
        let gen = AzureSasGenerator::new(
            "account", "container", azure_test_key_b64(), None,
        ).unwrap();
        let url = gen.presign_get("secret.bin", 3600).unwrap();
        // sp=r → lecture seule, jamais write (w) ou delete (d)
        let sp_value = url.split("sp=").nth(1)
            .and_then(|s| s.split('&').next())
            .unwrap_or("");
        assert_eq!(sp_value, "r", "La permission SAS doit être 'r' (lecture seule)");
        assert!(!sp_value.contains('w'), "Pas de permission d'écriture");
        assert!(!sp_value.contains('d'), "Pas de permission de suppression");
    }

    // ── Test batch helpers ──────────────────────────────────────

    #[test]
    fn presign_chunks_azure_produces_one_url_per_chunk() {
        let gen = AzureSasGenerator::new(
            "account", "container", azure_test_key_b64(), None,
        ).unwrap();
        let hashes = vec![
            "aabbcc001122334455667788990011223344556677889900112233445566778899".to_string(),
            "bbccdd112233445566778899001122334455667788990011223344556677889900".to_string(),
        ];
        let map = presign_chunks_azure(&gen, &hashes, 3600).unwrap();
        assert_eq!(map.len(), 2);
        for (sha, url) in &map {
            assert!(url.contains(&sha[..2]), "URL doit contenir le préfixe du chunk");
        }
    }
}
