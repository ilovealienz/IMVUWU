//! IMVU product lookup, revision discovery and revision downloads.
//!
//! Endpoints were checked live on 3 November 2026 with products 69190922 and
//! 29030566:
//! - `api.imvu.com/product/product-{id}` returns JSON, with the product under
//!   `denormalized[<that url>].data` and its parent in `.relations.parent`.
//!   An unknown product answers **401 with an empty body**, the same as a
//!   restricted one, so 401 is reported as "unavailable", never as a
//!   confirmed AP/hidden product.
//! - `userimages-akm.imvu.com/productdata/{id}/{rev}/_contents.json` lists a
//!   revision's files: `name`, optional hashed `url`, `tags`, and
//!   `original_dimensions`. A missing revision answers 404.
//! - Files live at `productdata/{id}/{rev}/{url or name}`.
//!
//! IMVU publishes no revision list here, so revisions are discovered by
//! checking numbers one by one. A scan can miss revisions after a long gap;
//! callers must show the range that was actually checked.
//!
//! Everything returned by these endpoints is untrusted data. Names are
//! validated before use, and response sizes are bounded.

use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

pub const API: &str = "https://api.imvu.com";
pub const PRODUCT_DATA: &str = "https://userimages-akm.imvu.com/productdata";

const METADATA_LIMIT: u64 = 2 * 1024 * 1024;
const CONTENTS_LIMIT: u64 = 2 * 1024 * 1024;
/// Largest single product file accepted (IMVU meshes are a few MB).
pub const FILE_LIMIT: u64 = 64 * 1024 * 1024;
/// Largest total for one downloaded revision held in memory.
pub const REVISION_LIMIT: u64 = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;

/// Accepts a bare product ID, a shop URL with `products_id=`, or an API URL
/// ending in `product-{id}`.
pub fn parse_product_input(input: &str) -> Option<u64> {
    let input = input.trim();
    let digits = |s: &str| -> Option<u64> {
        let d: String = s.chars().take_while(char::is_ascii_digit).collect();
        d.parse().ok().filter(|&n| n > 0)
    };
    if let Some(rest) = input.split("products_id=").nth(1) {
        return digits(rest);
    }
    if let Some(rest) = input
        .rsplit("product-")
        .next()
        .filter(|_| input.contains("product-"))
    {
        return digits(rest);
    }
    input
        .chars()
        .all(|c| c.is_ascii_digit())
        .then(|| digits(input))
        .flatten()
}

/// Metadata shown for a product. Every field is optional: the API may omit
/// any of them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProductInfo {
    pub id: u64,
    pub name: Option<String>,
    pub creator: Option<String>,
    pub creator_id: Option<u64>,
    pub rating: Option<String>,
    pub price: Option<String>,
    pub image_url: Option<String>,
    pub categories: Vec<String>,
    pub parent: Option<u64>,
    pub allows_derivation: Option<bool>,
    pub visible: Option<bool>,
    /// The API's `is` list: what kind of product this is, e.g.
    /// `avatar_attachment`, `clothing_product`.
    pub types: Vec<String>,
    /// `male`, `female` or another value the API gives.
    pub gender: Option<String>,
    pub bundle: Option<bool>,
}

const SHOP: &str = "https://www.imvu.com/shop";

impl ProductInfo {
    pub fn shop_url(&self) -> String {
        format!("{SHOP}/product.php?products_id={}", self.id)
    }
    /// IMVU's derivation tree page: this product's ancestors and descendants.
    pub fn derivation_tree_url(&self) -> String {
        format!("{SHOP}/derivation_tree.php?products_id={}", self.id)
    }
    /// Shop search for products derived from this one.
    pub fn derived_products_url(&self) -> String {
        format!("{SHOP}/web_search.php?derived_from={}", self.id)
    }
    pub fn creator_shop_url(&self) -> Option<String> {
        self.creator_id
            .map(|cid| format!("{SHOP}/web_search.php?manufacturers_id={cid}"))
    }
    pub fn api_url(&self) -> String {
        product_url(self.id)
    }
    /// A readable product type, e.g. "Avatar attachment · Clothing · male".
    /// Built only from what the API states; empty when it states nothing.
    pub fn type_summary(&self) -> String {
        let mut parts: Vec<String> = self
            .types
            .iter()
            .map(|t| {
                let words = t.trim_end_matches("_product").replace('_', " ");
                let mut chars = words.chars();
                chars
                    .next()
                    .map(|c| c.to_uppercase().chain(chars).collect())
                    .unwrap_or_default()
            })
            .filter(|t: &String| !t.is_empty())
            .collect();
        if self.bundle == Some(true) {
            parts.push("Bundle".into());
        }
        if let Some(g) = &self.gender {
            parts.push(g.clone());
        }
        parts.join(" · ")
    }
}

/// Why something could not be fetched. Kept specific so the UI never turns
/// a failure into a claim about the product.
#[derive(Clone, Debug, PartialEq)]
pub enum Failure {
    /// The server answered with this status. For products, 401 covers both
    /// "does not exist" and "restricted"; the API does not say which.
    Http(u16),
    /// No usable answer: DNS, TLS, timeout, connection reset…
    Network(String),
    /// An answer arrived but was not in the expected shape.
    Format(String),
    /// The answer was larger than the allowed limit.
    TooLarge,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(401) => write!(f, "unavailable (HTTP 401: unknown or restricted)"),
            Self::Http(404) => write!(f, "not found (HTTP 404)"),
            Self::Http(code) => write!(f, "HTTP {code}"),
            Self::Network(e) => write!(f, "network error: {e}"),
            Self::Format(e) => write!(f, "unexpected response: {e}"),
            Self::TooLarge => write!(f, "response too large"),
        }
    }
}

fn product_url(id: u64) -> String {
    format!("{API}/product/product-{id}")
}

fn id_from_resource(url: &str) -> Option<u64> {
    url.rsplit("product-").next()?.parse().ok()
}

/// Reads a product document from the REST API's JSON envelope.
pub fn parse_product(id: u64, json: &[u8]) -> Result<ProductInfo, Failure> {
    let value: serde_json::Value =
        serde_json::from_slice(json).map_err(|e| Failure::Format(e.to_string()))?;
    if value.get("status").and_then(|s| s.as_str()) != Some("success") {
        return Err(Failure::Format("status is not success".into()));
    }
    let entry = value
        .get("denormalized")
        .and_then(|d| d.get(product_url(id)))
        .ok_or_else(|| Failure::Format("product document missing".into()))?;
    let data = entry.get("data").unwrap_or(&serde_json::Value::Null);
    let text = |key: &str| -> Option<String> {
        match data.get(key)? {
            serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    };
    let flag = |key: &str| -> Option<bool> {
        match data.get(key)? {
            serde_json::Value::Bool(b) => Some(*b),
            serde_json::Value::Number(n) => n.as_u64().map(|n| n != 0),
            _ => None,
        }
    };
    Ok(ProductInfo {
        id,
        name: text("product_name"),
        creator: text("creator_name"),
        creator_id: data.get("creator_cid").and_then(|c| c.as_u64()),
        rating: text("rating"),
        price: text("product_price"),
        image_url: text("product_image").filter(|u| u.starts_with("https://")),
        categories: data
            .get("categories")
            .and_then(|c| c.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        parent: entry
            .get("relations")
            .and_then(|r| r.get("parent"))
            .and_then(|p| p.as_str())
            .and_then(id_from_resource),
        allows_derivation: flag("allows_derivation"),
        visible: flag("is_visible"),
        types: data
            .get("is")
            .and_then(|c| c.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        gender: text("gender"),
        bundle: flag("is_bundle"),
    })
}

/// One file listed in a revision's `_contents.json`.
#[derive(Clone, Debug, PartialEq)]
pub struct ContentsEntry {
    /// The original manifest name. Always used for saving.
    pub name: String,
    /// Hashed CDN filename, when the file is stored under a different name.
    pub url: Option<String>,
    pub tags: Vec<String>,
    pub original_dimensions: Option<String>,
}

impl ContentsEntry {
    /// The path segment fetched from the CDN.
    pub fn remote_name(&self) -> &str {
        self.url.as_deref().unwrap_or(&self.name)
    }
}

/// A plain filename: no directories, drive letters, `..`, control
/// characters or characters Windows rejects in names.
pub fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.ends_with('.')
        && !name.ends_with(' ')
        && !name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
}

pub fn parse_contents(json: &[u8]) -> Result<Vec<ContentsEntry>, Failure> {
    let value: serde_json::Value =
        serde_json::from_slice(json).map_err(|e| Failure::Format(e.to_string()))?;
    let list = value
        .as_array()
        .ok_or_else(|| Failure::Format("_contents.json is not a list".into()))?;
    if list.len() > MAX_ENTRIES {
        return Err(Failure::Format(format!(
            "{} entries is too many",
            list.len()
        )));
    }
    let mut entries = Vec::with_capacity(list.len());
    let mut seen = std::collections::BTreeSet::new();
    for item in list {
        let name = item
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| Failure::Format("entry without a name".into()))?;
        let url = item.get("url").and_then(|u| u.as_str()).map(str::to_string);
        if !is_safe_name(name) || url.as_deref().is_some_and(|u| !is_safe_name(u)) {
            return Err(Failure::Format(format!("unsafe file name {name:?}")));
        }
        // Windows names are case-insensitive: two entries must not collide.
        if !seen.insert(name.to_lowercase()) {
            return Err(Failure::Format(format!("duplicate file name {name:?}")));
        }
        entries.push(ContentsEntry {
            name: name.to_string(),
            url,
            tags: item
                .get("tags")
                .and_then(|t| t.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            original_dimensions: item
                .get("original_dimensions")
                .and_then(|d| d.as_str())
                .map(str::to_string),
        });
    }
    Ok(entries)
}

/// Percent-encodes one URL path segment (names may contain spaces).
fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn contents_url(id: u64, revision: u32) -> String {
    format!("{PRODUCT_DATA}/{id}/{revision}/_contents.json")
}

pub fn file_url(id: u64, revision: u32, entry: &ContentsEntry) -> String {
    format!(
        "{PRODUCT_DATA}/{id}/{revision}/{}",
        encode_segment(entry.remote_name())
    )
}

/// Shared HTTP client. Cheap to clone; safe to use from worker threads.
#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
}

impl Default for Client {
    fn default() -> Self {
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::Rustls)
            .build();
        let config = ureq::Agent::config_builder()
            .tls_config(tls)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .max_redirects(3)
            .user_agent(concat!("IMVUWU/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

impl Client {
    /// GET with a size limit; non-2xx statuses become `Failure::Http`.
    pub fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, Failure> {
        self.get_typed(url, limit).map(|(bytes, _)| bytes)
    }

    /// GET returning the body and the server's `Content-Type` (without
    /// parameters such as `charset`). The type is informational only: file
    /// kinds are always decided from the bytes.
    pub fn get_typed(&self, url: &str, limit: u64) -> Result<(Vec<u8>, Option<String>), Failure> {
        let mut response = self
            .agent
            .get(url)
            .call()
            .map_err(|e| Failure::Network(e.to_string()))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(Failure::Http(status));
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty() && v.len() < 100);
        let bytes = response
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map_err(|e| match e {
                ureq::Error::BodyExceedsLimit(_) => Failure::TooLarge,
                e => Failure::Network(e.to_string()),
            })?;
        Ok((bytes, content_type))
    }

    /// HEAD request; returns the status code.
    pub fn head(&self, url: &str) -> Result<u16, Failure> {
        self.agent
            .head(url)
            .call()
            .map(|r| r.status().as_u16())
            .map_err(|e| Failure::Network(e.to_string()))
    }

    pub fn product(&self, id: u64) -> Result<ProductInfo, Failure> {
        parse_product(id, &self.get(&product_url(id), METADATA_LIMIT)?)
    }

    pub fn contents(&self, id: u64, revision: u32) -> Result<Vec<ContentsEntry>, Failure> {
        parse_contents(&self.get(&contents_url(id, revision), CONTENTS_LIMIT)?)
    }
}

/// What a revision check found.
#[derive(Clone, Debug, PartialEq)]
pub enum RevisionState {
    Exists,
    Missing,
    /// Neither confirmed nor ruled out.
    Unknown(Failure),
}

/// Progress messages from a running revision scan.
#[derive(Clone, Debug, PartialEq)]
pub enum ScanEvent {
    Checked(u32, RevisionState),
    /// The scan ended. `last_checked` is the highest number checked with
    /// every lower number in the range also checked.
    Finished {
        last_checked: u32,
        stopped_early: bool,
    },
}

/// How far a scan goes.
#[derive(Clone, Copy, Debug)]
pub struct ScanPlan {
    pub first: u32,
    /// Hard upper bound, inclusive.
    pub last: u32,
    /// Stop after this many confirmed-missing numbers in a row beyond the
    /// highest revision found (unknown results do not count as missing).
    pub stop_after_missing: u32,
}

impl Default for ScanPlan {
    fn default() -> Self {
        Self {
            first: 1,
            last: 200,
            stop_after_missing: 15,
        }
    }
}

/// Decides, from results so far, whether a scan can stop.
/// `results[i]` is the state of revision `plan.first + i`; it must hold
/// the contiguous prefix of the range that has been checked.
pub fn scan_should_stop(plan: &ScanPlan, results: &[RevisionState]) -> bool {
    let mut run = 0;
    for state in results.iter().rev() {
        match state {
            RevisionState::Missing => run += 1,
            _ => break,
        }
    }
    let any_found = results.contains(&RevisionState::Exists);
    // With nothing found yet, keep looking a while longer before giving up.
    let needed = if any_found {
        plan.stop_after_missing
    } else {
        plan.stop_after_missing.max(30)
    };
    run >= needed
}

/// Checks revisions with a few concurrent HEAD requests, sending a
/// `ScanEvent` for each. Setting `cancel` stops it. Workers claim numbers in
/// order and the stop rule is evaluated on the contiguous checked prefix.
pub fn scan_revisions(
    client: Client,
    id: u64,
    plan: ScanPlan,
    cancel: Arc<AtomicBool>,
    events: mpsc::Sender<ScanEvent>,
) {
    const WORKERS: u32 = 6;
    let next = Arc::new(AtomicU32::new(plan.first));
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<(u32, RevisionState)>();
    let mut workers = Vec::new();
    for _ in 0..WORKERS {
        let (client, next, stop, cancel, tx) = (
            client.clone(),
            next.clone(),
            stop.clone(),
            cancel.clone(),
            tx.clone(),
        );
        workers.push(std::thread::spawn(move || loop {
            if stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
                break;
            }
            let revision = next.fetch_add(1, Ordering::Relaxed);
            if revision > plan.last {
                break;
            }
            let state = match client.head(&contents_url(id, revision)) {
                Ok(200..=299) => RevisionState::Exists,
                Ok(403 | 404) => RevisionState::Missing,
                Ok(code) => RevisionState::Unknown(Failure::Http(code)),
                Err(e) => RevisionState::Unknown(e),
            };
            if tx.send((revision, state)).is_err() {
                break;
            }
        }));
    }
    drop(tx);
    let mut pending = std::collections::BTreeMap::new();
    let mut prefix: Vec<RevisionState> = Vec::new();
    for (revision, state) in rx {
        let _ = events.send(ScanEvent::Checked(revision, state.clone()));
        pending.insert(revision, state);
        while let Some(state) = pending.remove(&(plan.first + prefix.len() as u32)) {
            prefix.push(state);
        }
        if scan_should_stop(&plan, &prefix) {
            stop.store(true, Ordering::Relaxed);
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
    let last_checked = plan.first + prefix.len() as u32 - 1;
    let _ = events.send(ScanEvent::Finished {
        last_checked: last_checked.max(plan.first.saturating_sub(1)),
        stopped_early: cancel.load(Ordering::Relaxed),
    });
}

/// One file of a downloaded revision.
#[derive(Clone, Debug)]
pub struct DownloadedFile {
    pub entry: ContentsEntry,
    pub result: Result<Vec<u8>, Failure>,
    /// The server's `Content-Type`, when the file was fetched.
    pub content_type: Option<String>,
}

/// Downloads every file of a revision with a few concurrent requests,
/// keeping manifest order. Stops early when `cancel` is set or the revision
/// total would exceed `REVISION_LIMIT`; files not fetched are reported.
pub fn download_revision(
    client: &Client,
    id: u64,
    revision: u32,
    entries: &[ContentsEntry],
    cancel: &AtomicBool,
    progress: &(dyn Fn(usize) + Sync),
) -> Vec<DownloadedFile> {
    const WORKERS: usize = 6;
    let total = std::sync::atomic::AtomicU64::new(0);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::atomic::AtomicUsize::new(0);
    type Fetched = Result<(Vec<u8>, Option<String>), Failure>;
    let results: Vec<std::sync::Mutex<Option<Fetched>>> = entries
        .iter()
        .map(|_| std::sync::Mutex::new(None))
        .collect();
    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(entries.len()) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(entry) = entries.get(i) else { break };
                let result = if cancel.load(Ordering::Relaxed) {
                    Err(Failure::Network("cancelled".into()))
                } else {
                    client
                        .get_typed(&file_url(id, revision, entry), FILE_LIMIT)
                        .and_then(|(bytes, content_type)| {
                            let sum = total.fetch_add(bytes.len() as u64, Ordering::Relaxed)
                                + bytes.len() as u64;
                            if sum > REVISION_LIMIT {
                                Err(Failure::TooLarge)
                            } else {
                                Ok((bytes, content_type))
                            }
                        })
                };
                *results[i].lock().unwrap() = Some(result);
                progress(done.fetch_add(1, Ordering::Relaxed) + 1);
            });
        }
    });
    entries
        .iter()
        .zip(results)
        .map(|(entry, result)| {
            let result = result
                .into_inner()
                .unwrap()
                .unwrap_or(Err(Failure::Network("not attempted".into())));
            let content_type = result.as_ref().ok().and_then(|(_, t)| t.clone());
            DownloadedFile {
                entry: entry.clone(),
                result: result.map(|(bytes, _)| bytes),
                content_type,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_input_accepts_ids_and_urls_only() {
        assert_eq!(parse_product_input(" 69190922 "), Some(69190922));
        assert_eq!(
            parse_product_input("https://www.imvu.com/shop/product.php?products_id=29030566&x=1"),
            Some(29030566)
        );
        assert_eq!(
            parse_product_input("https://api.imvu.com/product/product-44379985"),
            Some(44379985)
        );
        assert_eq!(parse_product_input("0"), None);
        assert_eq!(parse_product_input("12ab"), None);
        assert_eq!(parse_product_input("hello"), None);
    }

    #[test]
    fn product_document_reads_fields_and_parent() {
        // Shape observed live for product 69190922 (values shortened).
        let json = br#"{"status":"success","id":"https://api.imvu.com/product/product-69190922",
          "denormalized":{"https://api.imvu.com/product/product-69190922":{
            "data":{"product_name":"Glow","creator_name":"Maker","creator_cid":86959627,
                    "rating":"GA","product_price":120,"product_image":"https://example.invalid/a.png",
                    "categories":["Accessories","Particles"],"allows_derivation":1,"is_visible":true,
                    "is":["avatar_attachment","clothing_product"],"gender":"male","is_bundle":false},
            "relations":{"parent":"https://api.imvu.com/product/product-44379985"}}}}"#;
        let p = parse_product(69190922, json).unwrap();
        assert_eq!(p.name.as_deref(), Some("Glow"));
        assert_eq!(p.creator_id, Some(86959627));
        assert_eq!(p.price.as_deref(), Some("120"));
        assert_eq!(p.parent, Some(44379985));
        assert_eq!(p.categories.len(), 2);
        assert_eq!(p.allows_derivation, Some(true));
        assert_eq!(p.visible, Some(true));
        assert_eq!(p.type_summary(), "Avatar attachment · Clothing · male");
        assert_eq!(
            p.derivation_tree_url(),
            "https://www.imvu.com/shop/derivation_tree.php?products_id=69190922"
        );
        assert_eq!(
            p.creator_shop_url().as_deref(),
            Some("https://www.imvu.com/shop/web_search.php?manufacturers_id=86959627")
        );
        // A document for another product is not silently accepted.
        assert!(parse_product(1, json).is_err());
        assert!(parse_product(1, b"").is_err());
    }

    #[test]
    fn failures_never_claim_ap_or_hidden() {
        for f in [
            Failure::Http(401),
            Failure::Http(404),
            Failure::Http(500),
            Failure::Network("x".into()),
        ] {
            let text = f.to_string().to_lowercase();
            assert!(
                !text.contains("ap") || text.contains("unavailable"),
                "{text}"
            );
            assert!(!text.contains("hidden"));
        }
    }

    #[test]
    fn contents_keep_original_names_and_reject_unsafe_ones() {
        // Live shape for 29030566 revision 4.
        let json = br#"[{"url":"c7037f5e3eb3abc6164cf41111347efd.xmf","name":"Blaise3.xmf"},
            {"name":"particle.png","tags":["original","thumbnail"],"original_dimensions":"256,512"},
            {"name":"index.xml"}]"#;
        let entries = parse_contents(json).unwrap();
        assert_eq!(entries[0].name, "Blaise3.xmf");
        assert_eq!(
            entries[0].remote_name(),
            "c7037f5e3eb3abc6164cf41111347efd.xmf"
        );
        assert_eq!(entries[1].remote_name(), "particle.png");
        assert_eq!(entries[1].original_dimensions.as_deref(), Some("256,512"));
        for bad in [
            r#"[{"name":"../x.png"}]"#,
            r#"[{"name":"a/b.png"}]"#,
            r#"[{"name":"C:x"}]"#,
            r#"[{"name":"ok.png","url":"..\\evil"}]"#,
            r#"[{"name":"A.png"},{"name":"a.PNG"}]"#,
            r#"{"name":"x"}"#,
        ] {
            assert!(parse_contents(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn file_urls_encode_names() {
        let entry = ContentsEntry {
            name: "Cargo Shorties.jpg".into(),
            url: None,
            tags: vec![],
            original_dimensions: None,
        };
        assert_eq!(
            file_url(5, 2, &entry),
            format!("{PRODUCT_DATA}/5/2/Cargo%20Shorties.jpg")
        );
    }

    #[test]
    fn scan_stops_only_after_confirmed_missing_run() {
        use RevisionState::*;
        let plan = ScanPlan {
            first: 1,
            last: 200,
            stop_after_missing: 3,
        };
        assert!(!scan_should_stop(&plan, &[Exists, Missing, Missing]));
        assert!(scan_should_stop(
            &plan,
            &[Exists, Missing, Missing, Missing]
        ));
        // An unknown result breaks the run: it is not evidence of absence.
        let unknown = Unknown(Failure::Network("t".into()));
        assert!(!scan_should_stop(
            &plan,
            &[Exists, Missing, Missing, unknown.clone(), Missing, Missing]
        ));
        // Nothing found yet: needs a longer run before giving up.
        assert!(!scan_should_stop(&plan, &vec![Missing; 29]));
        assert!(scan_should_stop(&plan, &vec![Missing; 30]));
    }
}
