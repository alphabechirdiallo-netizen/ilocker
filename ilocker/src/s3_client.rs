// ============================================================
//  s3_client.rs — AWS Signature V4 S3 client  (Phase 3)
//
//  Implements just enough of the S3 API for ilocker:
//    • PUT  Object  (upload one chunk)
//    • GET  Object  (download one chunk)
//    • HEAD Object  (check if chunk exists — deduplication)
//    • DELETE Object (optional cleanup)
//
//  Uses hyper-rustls for TLS (avoids openssl-sys) and
//  computes AWS Signature V4 manually using sha2 + hmac.
//
//  S3-compatible:
//    Works with AWS S3, Backblaze B2, and MinIO by accepting
//    a custom endpoint URL.
//
//  References:
//    https://docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-authenticating-requests.html
// ============================================================

use anyhow::{bail, Context, Result};
use chrono::Utc;
use hmac::{Hmac, Mac};
use hyper::{Body, Client, Method, Request, StatusCode, Uri};
use hyper_rustls::HttpsConnectorBuilder;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

// ── Client ────────────────────────────────────────────────────

pub struct S3Client {
    bucket:     String,
    region:     String,
    endpoint:   String,     // e.g. "https://s3.amazonaws.com" or MinIO URL
    access_key: String,
    secret_key: String,
}

impl S3Client {
    /// Create a client from explicit credentials.
    /// `endpoint` defaults to the standard AWS endpoint if None.
    pub fn new(
        bucket:     &str,
        region:     &str,
        endpoint:   Option<&str>,
        access_key: &str,
        secret_key: &str,
    ) -> Self {
        let endpoint = endpoint
            .map(|e| e.trim_end_matches('/').to_string())
            .unwrap_or_else(|| format!("https://s3.{}.amazonaws.com", region));

        Self {
            bucket:     bucket.to_string(),
            region:     region.to_string(),
            endpoint,
            access_key: access_key.to_string(),
            secret_key: secret_key.to_string(),
        }
    }

    /// Build from resolved CloudCredentials (convenience).
    pub fn from_creds(creds: &crate::cloud_store::CloudCredentials) -> Self {
        Self::new(
            &creds.bucket,
            &creds.region,
            creds.endpoint.as_deref(),
            &creds.access_key_id,
            &creds.secret_access_key,
        )
    }

    // ── Object key helpers ────────────────────────────────────

    /// Object key for a chunk in the bucket: chunks/<sha[..2]>/<sha>
    pub fn chunk_key(sha256: &str) -> String {
        format!("chunks/{}/{}", &sha256[..2], sha256)
    }

    // ── API methods ───────────────────────────────────────────

    /// Check if a chunk already exists in the bucket (HEAD request).
    /// Returns true if the object is present, false if 404.
    pub async fn chunk_exists(&self, sha256: &str) -> Result<bool> {
        let key    = Self::chunk_key(sha256);
        let status = self.head_object(&key).await?;
        match status {
            StatusCode::OK        => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            s                     => bail!("HEAD {} returned unexpected status {}", key, s),
        }
    }

    /// Upload a chunk (PUT).  Returns the ETag on success.
    pub async fn put_chunk(&self, sha256: &str, data: &[u8]) -> Result<()> {
        let key = Self::chunk_key(sha256);
        self.put_object(&key, data, "application/octet-stream").await
    }

    /// Download a chunk (GET).  Returns raw bytes.
    pub async fn get_chunk(&self, sha256: &str) -> Result<Vec<u8>> {
        let key = Self::chunk_key(sha256);
        self.get_object(&key).await
    }

    // ── Wrappers génériques (clé arbitraire — manifests, healthcheck) ──

    pub async fn exists(&self, key: &str) -> Result<bool> {
        match self.head_object(key).await? {
            StatusCode::OK        => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            s                     => bail!("HEAD {} returned unexpected status {}", key, s),
        }
    }

    pub async fn put_raw(&self, key: &str, data: &[u8]) -> Result<()> {
        self.put_object(key, data, "application/octet-stream").await
    }

    pub async fn get_raw(&self, key: &str) -> Result<Vec<u8>> {
        self.get_object(key).await
    }

    pub async fn delete_raw(&self, key: &str) -> Result<()> {
        self.delete_object(key).await
    }

    /// Liste TOUS les objets sous un préfixe donné (pagination
    /// automatique via ListObjectsV2). Retourne (clé, taille en octets).
    pub async fn list_all(&self, prefix: &str) -> Result<Vec<(String, u64)>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let (page, next) = self.list_objects_page(prefix, token.as_deref()).await?;
            out.extend(page);
            match next {
                Some(t) => token = Some(t),
                None    => break,
            }
        }
        Ok(out)
    }

    // ── Low-level signed requests ─────────────────────────────

    async fn delete_object(&self, key: &str) -> Result<()> {
        let (uri, host) = self.object_uri(key)?;
        let now        = Utc::now();
        let date_str   = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_short = &date_str[..8];

        let headers = self.sign(
            Method::DELETE, key, &host, &date_str, date_short,
            &[], "application/octet-stream", false, "",
        )?;

        let mut builder = Request::builder().method(Method::DELETE).uri(uri);
        for (k, v) in &headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let req = builder.body(Body::empty())?;
        let resp = self.https_client().request(req).await
            .context("S3 DELETE request failed")?;

        if !resp.status().is_success() && resp.status() != StatusCode::NOT_FOUND {
            bail!("S3 DELETE {} returned {}", key, resp.status());
        }
        Ok(())
    }

    /// Une page de ListObjectsV2 (max 1000 clés par défaut côté S3).
    /// Retourne (objets, continuation_token_suivant).
    async fn list_objects_page(
        &self,
        prefix: &str,
        continuation_token: Option<&str>,
    ) -> Result<(Vec<(String, u64)>, Option<String>)> {
        let mut query_pairs: Vec<(String, String)> = vec![
            ("list-type".to_string(), "2".to_string()),
            ("prefix".to_string(),    prefix.to_string()),
        ];
        if let Some(tok) = continuation_token {
            query_pairs.push(("continuation-token".to_string(), tok.to_string()));
        }
        query_pairs.sort_by(|a, b| a.0.cmp(&b.0));

        let canonical_query: String = query_pairs.iter()
            .map(|(k, v)| format!("{}={}", uri_encode(k, true), uri_encode(v, true)))
            .collect::<Vec<_>>()
            .join("&");

        let request_query: String = query_pairs.iter()
            .map(|(k, v)| format!("{}={}", uri_encode(k, true), uri_encode(v, true)))
            .collect::<Vec<_>>()
            .join("&");

        let url = format!("{}/{}?{}", self.endpoint, self.bucket, request_query);
        let uri: Uri = url.parse().context("Invalid S3 list URL")?;
        let host = uri.host().ok_or_else(|| anyhow::anyhow!("No host in S3 URL"))?.to_string();

        let now        = Utc::now();
        let date_str   = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_short = &date_str[..8];

        let headers = self.sign(
            Method::GET, "", &host, &date_str, date_short,
            &[], "application/octet-stream", false, &canonical_query,
        )?;

        let mut builder = Request::builder().method(Method::GET).uri(uri);
        for (k, v) in &headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let req  = builder.body(Body::empty())?;
        let resp = self.https_client().request(req).await
            .context("S3 ListObjectsV2 request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body   = hyper::body::to_bytes(resp.into_body()).await.unwrap_or_default();
            bail!("S3 ListObjectsV2 returned {}: {}", status, String::from_utf8_lossy(&body));
        }

        let bytes = hyper::body::to_bytes(resp.into_body()).await
            .context("Failed to read ListObjectsV2 response body")?;
        let xml = String::from_utf8_lossy(&bytes);
        Ok(parse_list_objects_xml(&xml))
    }

    async fn head_object(&self, key: &str) -> Result<StatusCode> {
        let (uri, host) = self.object_uri(key)?;
        let now         = Utc::now();
        let date_str    = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_short  = &date_str[..8];

        let headers = self.sign(
            Method::HEAD, key, &host, &date_str, date_short,
            &[], "application/octet-stream", false, "",
        )?;

        let mut builder = Request::builder()
            .method(Method::HEAD)
            .uri(uri);
        for (k, v) in &headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let req = builder.body(Body::empty())?;

        let client = self.https_client();
        let resp   = client.request(req).await
            .context("S3 HEAD request failed")?;
        Ok(resp.status())
    }

    async fn put_object(&self, key: &str, data: &[u8], content_type: &str) -> Result<()> {
        let (uri, host) = self.object_uri(key)?;
        let now         = Utc::now();
        let date_str    = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_short  = &date_str[..8];

        let headers = self.sign(
            Method::PUT, key, &host, &date_str, date_short,
            data, content_type, true, "",
        )?;

        let mut builder = Request::builder()
            .method(Method::PUT)
            .uri(uri);
        for (k, v) in &headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let req = builder.body(Body::from(data.to_vec()))?;

        let client = self.https_client();
        let resp   = client.request(req).await
            .context("S3 PUT request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body   = hyper::body::to_bytes(resp.into_body()).await.unwrap_or_default();
            bail!("S3 PUT {} returned {}: {}", key, status,
                  String::from_utf8_lossy(&body));
        }
        Ok(())
    }

    async fn get_object(&self, key: &str) -> Result<Vec<u8>> {
        let (uri, host) = self.object_uri(key)?;
        let now         = Utc::now();
        let date_str    = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_short  = &date_str[..8];

        let headers = self.sign(
            Method::GET, key, &host, &date_str, date_short,
            &[], "application/octet-stream", false, "",
        )?;

        let mut builder = Request::builder()
            .method(Method::GET)
            .uri(uri);
        for (k, v) in &headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let req = builder.body(Body::empty())?;

        let client = self.https_client();
        let resp   = client.request(req).await
            .context("S3 GET request failed")?;

        if !resp.status().is_success() {
            bail!("S3 GET {} returned {}", key, resp.status());
        }

        let bytes = hyper::body::to_bytes(resp.into_body()).await
            .context("Failed to read S3 response body")?;
        Ok(bytes.to_vec())
    }

    // ── URI construction ──────────────────────────────────────

    fn object_uri(&self, key: &str) -> Result<(Uri, String)> {
        let url = format!("{}/{}/{}", self.endpoint, self.bucket, key);
        let uri: Uri = url.parse().context("Invalid S3 object URL")?;
        let host = uri.host()
            .ok_or_else(|| anyhow::anyhow!("No host in S3 URL"))?
            .to_string();
        Ok((uri, host))
    }

    // ── AWS Signature V4 ──────────────────────────────────────
    //
    // Returns a Vec of (header-name, header-value) pairs ready
    // to be added to the HTTP request.

    fn sign(
        &self,
        method:        Method,
        key:           &str,
        host:          &str,
        datetime:      &str,    // "20260101T120000Z"
        date_short:    &str,    // "20260101"
        body:          &[u8],
        content_type:  &str,
        include_ct:    bool,
        canonical_query: &str,
    ) -> Result<Vec<(String, String)>> {
        // 1. Canonical request
        let payload_hash = hex::encode(Sha256::digest(body));
        let canonical_path  = if key.is_empty() {
            format!("/{}", self.bucket)
        } else {
            format!("/{}/{}", self.bucket, key)
        };

        let mut signed_headers_list = vec![
            ("host",                  host.to_string()),
            ("x-amz-content-sha256", payload_hash.clone()),
            ("x-amz-date",           datetime.to_string()),
        ];
        if include_ct {
            signed_headers_list.push(("content-type", content_type.to_string()));
        }
        signed_headers_list.sort_by(|a, b| a.0.cmp(b.0));

        let canonical_headers: String = signed_headers_list
            .iter()
            .map(|(k, v)| format!("{}:{}\n", k, v.trim()))
            .collect();

        let signed_headers: String = signed_headers_list
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<_>>()
            .join(";");

        let canonical_request = format!(
            "{method}\n{path}\n{query}\n{headers}\n{signed}\n{hash}",
            method  = method.as_str(),
            path    = canonical_path,
            query   = canonical_query,
            headers = canonical_headers,
            signed  = signed_headers,
            hash    = payload_hash,
        );

        // 2. String to sign
        let scope = format!("{}/{}/s3/aws4_request", date_short, self.region);
        let cr_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{datetime}\n{scope}\n{cr_hash}"
        );

        // 3. Signing key
        let signing_key = {
            let date_key    = hmac_sha256(
                format!("AWS4{}", self.secret_key).as_bytes(),
                date_short.as_bytes(),
            )?;
            let region_key  = hmac_sha256(&date_key, self.region.as_bytes())?;
            let service_key = hmac_sha256(&region_key, b"s3")?;
            hmac_sha256(&service_key, b"aws4_request")?
        };

        // 4. Signature
        let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes())?);

        // 5. Authorization header
        let auth = format!(
            "AWS4-HMAC-SHA256 Credential={}/{},SignedHeaders={},Signature={}",
            self.access_key, scope, signed_headers, signature
        );

        let mut result = vec![
            ("Authorization".to_string(),        auth),
            ("Host".to_string(),                  host.to_string()),
            ("x-amz-date".to_string(),            datetime.to_string()),
            ("x-amz-content-sha256".to_string(),  payload_hash),
        ];
        if include_ct {
            result.push(("Content-Type".to_string(), content_type.to_string()));
        }
        Ok(result)
    }

    fn https_client(&self) -> Client<hyper_rustls::HttpsConnector<hyper::client::HttpConnector>> {
        // with_native_roots() — pas with_webpki_roots(). Incohérence corrigée :
        // même correctif déjà appliqué à github_client.rs, vercel_client.rs,
        // supabase_client.rs, updater.rs et provider_engine.rs (liste de CA figée
        // à la compilation qui échoue derrière tout proxy d'entreprise/inspection TLS).
        let connector = HttpsConnectorBuilder::new()
            .with_native_roots()
            .https_or_http()
            .enable_http1()
            .build();
        Client::builder().build(connector)
    }
}

// ── HMAC-SHA256 helper ────────────────────────────────────────

fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|e| anyhow::anyhow!("HMAC key error: {}", e))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

// ── URI encoding (RFC 3986) — requis par la spec SigV4 pour la
//    canonicalisation de la query string ────────────────────────

fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

// ── Parseur minimal de réponse ListObjectsV2 ─────────────────
//
// Pas de dépendance XML (binaire standalone) : la réponse S3 est
// suffisamment simple et stable pour une extraction par
// délimiteurs. Retourne (objets, continuation_token_suivant).

fn parse_list_objects_xml(xml: &str) -> (Vec<(String, u64)>, Option<String>) {
    let mut objects = Vec::new();

    for block in xml.split("<Contents>").skip(1) {
        let end = block.find("</Contents>").unwrap_or(block.len());
        let entry = &block[..end];

        let key = extract_tag(entry, "Key").map(|k| xml_unescape(&k));
        let size = extract_tag(entry, "Size").and_then(|s| s.parse::<u64>().ok());

        if let (Some(key), Some(size)) = (key, size) {
            objects.push((key, size));
        }
    }

    let next_token = if extract_tag(xml, "IsTruncated").as_deref() == Some("true") {
        extract_tag(xml, "NextContinuationToken")
    } else {
        None
    };

    (objects, next_token)
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
