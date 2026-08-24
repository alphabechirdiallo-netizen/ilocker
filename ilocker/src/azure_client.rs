// ============================================================
//  azure_client.rs — Azure Blob Storage client (Shared Key auth)
//
//  v1.14.0 — Support Azure
//  ─────────────────────────────────────────────────────────
//  Azure Blob Storage n'est PAS compatible avec l'API S3 (auth
//  différente, en-têtes différents, format de requêtes différent)
//  — ce module est donc un second client HTTP dédié, indépendant
//  de s3_client.rs, mais exposant exactement la même API publique
//  (chunk_exists, put_chunk, get_chunk, exists, put_raw, get_raw,
//  delete_raw, list_all) afin de pouvoir être utilisé de façon
//  interchangeable via CloudBackend (cloud_backend.rs).
//
//  Authentification : "Shared Key" (RFC officiel Microsoft)
//    https://learn.microsoft.com/en-us/rest/api/storageservices/authorize-with-shared-key
//
//  Le compte de stockage Azure (storage_account) joue le rôle du
//  "bucket"/"region" combinés ; le "container" joue le rôle du bucket
//  S3. On réutilise les champs CloudCredentials existants ainsi :
//    bucket   → nom du container Blob Storage
//    region   → nom du compte de stockage (storage account)
//    endpoint → URL personnalisée si Azure Government / sovereign cloud
//               (sinon : https://<account>.blob.core.windows.net)
// ============================================================

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use chrono::Utc;
use hmac::{Hmac, Mac};
use hyper::{Body, Client, Method, Request, StatusCode, Uri};
use hyper_rustls::HttpsConnectorBuilder;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const API_VERSION: &str = "2021-08-06";

pub struct AzureClient {
    account:      String,
    container:    String,
    endpoint:     String,       // ex: https://<account>.blob.core.windows.net
    account_key:  Vec<u8>,      // décodé depuis base64
}

impl AzureClient {
    /// Crée un client depuis des informations explicites.
    /// `account_key` est attendu encodé en base64 (format natif Azure,
    /// tel que fourni par le portail / CLI Azure).
    pub fn new(
        account:      &str,
        container:    &str,
        endpoint:     Option<&str>,
        account_key_b64: &str,
    ) -> Result<Self> {
        let endpoint = endpoint
            .map(|e| e.trim_end_matches('/').to_string())
            .unwrap_or_else(|| format!("https://{}.blob.core.windows.net", account));

        let account_key = B64.decode(account_key_b64)
            .context("Clé de compte Azure invalide — attendu en base64 (telle que fournie par le portail Azure)")?;

        Ok(Self {
            account: account.to_string(),
            container: container.to_string(),
            endpoint,
            account_key,
        })
    }

    /// Construit depuis des CloudCredentials résolues (convention :
    /// bucket=container, region=storage_account_name).
    pub fn from_creds(creds: &crate::cloud_store::CloudCredentials) -> Result<Self> {
        Self::new(
            &creds.region,                // storage account name
            &creds.bucket,                 // container name
            creds.endpoint.as_deref(),
            &creds.secret_access_key,      // access_key_id = nom du compte (déjà dans region), secret = clé
        )
    }

    pub fn chunk_key(sha256: &str) -> String {
        format!("chunks/{}/{}", &sha256[..2], sha256)
    }

    // ── API methods (miroir exact de S3Client) ──────────────────

    pub async fn chunk_exists(&self, sha256: &str) -> Result<bool> {
        self.exists(&Self::chunk_key(sha256)).await
    }

    pub async fn put_chunk(&self, sha256: &str, data: &[u8]) -> Result<()> {
        self.put_raw(&Self::chunk_key(sha256), data).await
    }

    pub async fn get_chunk(&self, sha256: &str) -> Result<Vec<u8>> {
        self.get_raw(&Self::chunk_key(sha256)).await
    }

    pub async fn exists(&self, key: &str) -> Result<bool> {
        match self.head_blob(key).await? {
            StatusCode::OK        => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            s                     => bail!("HEAD {} a renvoyé un statut inattendu {}", key, s),
        }
    }

    pub async fn put_raw(&self, key: &str, data: &[u8]) -> Result<()> {
        self.put_blob(key, data).await
    }

    pub async fn get_raw(&self, key: &str) -> Result<Vec<u8>> {
        self.get_blob(key).await
    }

    pub async fn delete_raw(&self, key: &str) -> Result<()> {
        self.delete_blob(key).await
    }

    /// Liste tous les blobs sous un préfixe (pagination via NextMarker).
    pub async fn list_all(&self, prefix: &str) -> Result<Vec<(String, u64)>> {
        let mut out = Vec::new();
        let mut marker: Option<String> = None;
        loop {
            let (page, next) = self.list_blobs_page(prefix, marker.as_deref()).await?;
            out.extend(page);
            match next {
                Some(m) => marker = Some(m),
                None    => break,
            }
        }
        Ok(out)
    }

    // ── Low-level signed requests ─────────────────────────────

    fn blob_url(&self, key: &str) -> String {
        format!("{}/{}/{}", self.endpoint, self.container, key)
    }

    fn https_client(&self) -> Client<hyper_rustls::HttpsConnector<hyper::client::HttpConnector>> {
        // with_native_roots() — pas with_webpki_roots(). Incohérence corrigée :
        // même correctif déjà appliqué à github_client.rs, vercel_client.rs,
        // supabase_client.rs, updater.rs et provider_engine.rs (liste de CA figée
        // à la compilation qui échoue derrière tout proxy d'entreprise/inspection TLS).
        let https = HttpsConnectorBuilder::new()
            .with_native_roots()
            .https_or_http()
            .enable_http1()
            .build();
        Client::builder().build(https)
    }

    async fn head_blob(&self, key: &str) -> Result<StatusCode> {
        let url  = self.blob_url(key);
        let uri: Uri = url.parse().context("URL Azure invalide")?;
        let now  = Utc::now();
        let date = now.format("%a, %d %b %Y %H:%M:%S GMT").to_string();

        let headers = self.sign(Method::HEAD, key, &date, 0, "")?;
        let mut builder = Request::builder().method(Method::HEAD).uri(uri);
        for (k, v) in &headers { builder = builder.header(k.as_str(), v.as_str()); }
        let req  = builder.body(Body::empty())?;
        let resp = self.https_client().request(req).await
            .context("Requête Azure HEAD échouée")?;
        Ok(resp.status())
    }

    async fn put_blob(&self, key: &str, data: &[u8]) -> Result<()> {
        let url  = self.blob_url(key);
        let uri: Uri = url.parse().context("URL Azure invalide")?;
        let now  = Utc::now();
        let date = now.format("%a, %d %b %Y %H:%M:%S GMT").to_string();

        let headers = self.sign(Method::PUT, key, &date, data.len(), "BlockBlob")?;
        let mut builder = Request::builder().method(Method::PUT).uri(uri);
        for (k, v) in &headers { builder = builder.header(k.as_str(), v.as_str()); }
        let req = builder.body(Body::from(data.to_vec()))?;
        let resp = self.https_client().request(req).await
            .context("Requête Azure PUT échouée")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body   = hyper::body::to_bytes(resp.into_body()).await.unwrap_or_default();
            bail!("Azure PUT {} a renvoyé {}: {}", key, status, String::from_utf8_lossy(&body));
        }
        Ok(())
    }

    async fn get_blob(&self, key: &str) -> Result<Vec<u8>> {
        let url  = self.blob_url(key);
        let uri: Uri = url.parse().context("URL Azure invalide")?;
        let now  = Utc::now();
        let date = now.format("%a, %d %b %Y %H:%M:%S GMT").to_string();

        let headers = self.sign(Method::GET, key, &date, 0, "")?;
        let mut builder = Request::builder().method(Method::GET).uri(uri);
        for (k, v) in &headers { builder = builder.header(k.as_str(), v.as_str()); }
        let req  = builder.body(Body::empty())?;
        let resp = self.https_client().request(req).await
            .context("Requête Azure GET échouée")?;

        if resp.status() == StatusCode::NOT_FOUND {
            bail!("Blob '{}' introuvable (404)", key);
        }
        if !resp.status().is_success() {
            bail!("Azure GET {} a renvoyé {}", key, resp.status());
        }

        let bytes = hyper::body::to_bytes(resp.into_body()).await
            .context("Échec de lecture du corps de la réponse Azure")?;
        Ok(bytes.to_vec())
    }

    async fn delete_blob(&self, key: &str) -> Result<()> {
        let url  = self.blob_url(key);
        let uri: Uri = url.parse().context("URL Azure invalide")?;
        let now  = Utc::now();
        let date = now.format("%a, %d %b %Y %H:%M:%S GMT").to_string();

        let headers = self.sign(Method::DELETE, key, &date, 0, "")?;
        let mut builder = Request::builder().method(Method::DELETE).uri(uri);
        for (k, v) in &headers { builder = builder.header(k.as_str(), v.as_str()); }
        let req  = builder.body(Body::empty())?;
        let resp = self.https_client().request(req).await
            .context("Requête Azure DELETE échouée")?;

        if !resp.status().is_success() && resp.status() != StatusCode::NOT_FOUND {
            bail!("Azure DELETE {} a renvoyé {}", key, resp.status());
        }
        Ok(())
    }

    /// Une page de l'API "List Blobs" (Container), avec pagination
    /// via NextMarker. Retourne (objets, marker_suivant).
    async fn list_blobs_page(
        &self,
        prefix: &str,
        marker: Option<&str>,
    ) -> Result<(Vec<(String, u64)>, Option<String>)> {
        let mut query = format!(
            "restype=container&comp=list&prefix={}",
            urlencode(prefix)
        );
        if let Some(m) = marker {
            query.push_str(&format!("&marker={}", urlencode(m)));
        }

        let url = format!("{}/{}?{}", self.endpoint, self.container, query);
        let uri: Uri = url.parse().context("URL Azure invalide")?;
        let now  = Utc::now();
        let date = now.format("%a, %d %b %Y %H:%M:%S GMT").to_string();

        let headers = self.sign_with_query(Method::GET, "", &date, 0, "", &query)?;
        let mut builder = Request::builder().method(Method::GET).uri(uri);
        for (k, v) in &headers { builder = builder.header(k.as_str(), v.as_str()); }
        let req  = builder.body(Body::empty())?;
        let resp = self.https_client().request(req).await
            .context("Requête Azure List Blobs échouée")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body   = hyper::body::to_bytes(resp.into_body()).await.unwrap_or_default();
            bail!("Azure List Blobs a renvoyé {}: {}", status, String::from_utf8_lossy(&body));
        }

        let bytes = hyper::body::to_bytes(resp.into_body()).await
            .context("Échec de lecture de la réponse List Blobs")?;
        let xml = String::from_utf8_lossy(&bytes);
        Ok(parse_list_blobs_xml(&xml))
    }

    // ── Signature "Shared Key" (RFC officiel Microsoft) ─────────
    //
    // StringToSign = VERB + "\n" +
    //     Content-Encoding + "\n" + Content-Language + "\n" +
    //     Content-Length + "\n" + Content-MD5 + "\n" + Content-Type + "\n" +
    //     Date + "\n" + If-Modified-Since + "\n" + If-Match + "\n" +
    //     If-None-Match + "\n" + If-Unmodified-Since + "\n" + Range + "\n" +
    //     CanonicalizedHeaders + CanonicalizedResource

    fn sign(
        &self,
        method:        Method,
        key:           &str,
        date:          &str,
        content_length: usize,
        blob_type:     &str,
    ) -> Result<Vec<(String, String)>> {
        self.sign_with_query(method, key, date, content_length, blob_type, "")
    }

    fn sign_with_query(
        &self,
        method:        Method,
        key:           &str,
        date:          &str,
        content_length: usize,
        blob_type:     &str,
        query:         &str,
    ) -> Result<Vec<(String, String)>> {
        let content_length_str = if content_length > 0 { content_length.to_string() } else { String::new() };

        let mut canonicalized_headers = format!("x-ms-date:{}\nx-ms-version:{}\n", date, API_VERSION);
        if !blob_type.is_empty() {
            // x-ms-blob-type doit être inséré dans l'ordre alphabétique
            canonicalized_headers = format!(
                "x-ms-blob-type:{}\nx-ms-date:{}\nx-ms-version:{}\n",
                blob_type, date, API_VERSION
            );
        }

        let canonicalized_resource = self.canonicalized_resource(key, query);

        let string_to_sign = format!(
            "{verb}\n\n\n{length}\n\n\n\n\n\n\n\n\n{headers}{resource}",
            verb     = method.as_str(),
            length   = content_length_str,
            headers  = canonicalized_headers,
            resource = canonicalized_resource,
        );

        let signature = self.hmac_sign(&string_to_sign)?;
        let auth = format!("SharedKey {}:{}", self.account, signature);

        let mut headers = vec![
            ("x-ms-date".to_string(), date.to_string()),
            ("x-ms-version".to_string(), API_VERSION.to_string()),
            ("Authorization".to_string(), auth),
            ("Host".to_string(), self.host_only()),
        ];
        if !blob_type.is_empty() {
            headers.push(("x-ms-blob-type".to_string(), blob_type.to_string()));
        }
        if content_length > 0 {
            headers.push(("Content-Length".to_string(), content_length.to_string()));
        }
        Ok(headers)
    }

    /// CanonicalizedResource = /<account>/<container>/<blob>\n<query triée>
    fn canonicalized_resource(&self, key: &str, query: &str) -> String {
        let mut resource = format!("/{}/{}", self.account, self.container);
        if !key.is_empty() {
            resource.push('/');
            resource.push_str(key);
        }

        if !query.is_empty() {
            // Les paramètres de requête doivent être triés par nom, en
            // minuscules, et formatés "nom:valeur" (un par ligne).
            let mut pairs: Vec<(String, String)> = query.split('&')
                .filter_map(|p| {
                    let mut it = p.splitn(2, '=');
                    let k = it.next()?.to_lowercase();
                    let v = it.next().unwrap_or("").to_string();
                    Some((k, v))
                })
                .collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            for (k, v) in pairs {
                resource.push_str(&format!("\n{}:{}", k, urldecode(&v)));
            }
        }

        resource
    }

    fn host_only(&self) -> String {
        self.endpoint
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .to_string()
    }

    fn hmac_sign(&self, string_to_sign: &str) -> Result<String> {
        let mut mac = HmacSha256::new_from_slice(&self.account_key)
            .map_err(|e| anyhow::anyhow!("Clé de compte Azure invalide pour HMAC: {}", e))?;
        mac.update(string_to_sign.as_bytes());
        Ok(B64.encode(mac.finalize().into_bytes()))
    }
}

// ── Helpers ───────────────────────────────────────────────────

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    // Décodage minimal (suffisant pour les valeurs de marker/prefix
    // qu'on encode nous-mêmes — pas un décodeur URL générique).
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                out.push(byte as char);
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Parseur minimal de la réponse XML "List Blobs" d'Azure.
/// Retourne (objets, marker_suivant).
fn parse_list_blobs_xml(xml: &str) -> (Vec<(String, u64)>, Option<String>) {
    let mut objects = Vec::new();

    for block in xml.split("<Blob>").skip(1) {
        let end = block.find("</Blob>").unwrap_or(block.len());
        let entry = &block[..end];

        let name = extract_tag(entry, "Name").map(|n| xml_unescape(&n));
        let size = extract_tag(entry, "Content-Length")
            .or_else(|| extract_tag(entry, "Content-Length")) // formats Azure variables
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        if let Some(name) = name {
            objects.push((name, size));
        }
    }

    let next_marker = extract_tag(xml, "NextMarker")
        .filter(|m| !m.is_empty());

    (objects, next_marker)
}

fn extract_tag(xml: &str, tag: &str) -> Option<String> {
    let open  = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = xml.find(&open)? + open.len();
    let end   = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_string())
}

fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}
