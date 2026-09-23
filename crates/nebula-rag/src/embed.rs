//! Embeddings: Workers AI `@cf/baai/bge-m3` over REST. Documents and
//! queries go in the same `{text: [...]}` shape with no instruction prefix
//! (what won the eval). The HTTP call shells out to `curl` like
//! `nebula upgrade` does, so the workspace gains no TLS stack; the token
//! reaches curl through its stdin config, never argv.

use crate::db::DIMS;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Texts per request. bge-m3 accepts more; small batches keep a failed
/// request cheap to retry.
pub const BATCH: usize = 32;

pub struct Embedded {
    pub vectors: Vec<Vec<f32>>,
    pub neurons: f64,
    pub tokens: u64,
}

pub trait Embedder {
    /// One vector per text, in order. Implementations may batch internally.
    fn embed(&self, texts: &[String]) -> Result<Embedded>;
}

/// Returned (inside anyhow) when the daily neuron budget would be exceeded
/// or Workers AI says the free allocation is used up. Sync stops and the
/// next run resumes where it left off.
#[derive(Debug)]
pub struct CapReached(pub String);

impl std::fmt::Display for CapReached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "daily embedding budget reached: {}", self.0)
    }
}
impl std::error::Error for CapReached {}

pub fn is_cap(e: &anyhow::Error) -> bool {
    e.downcast_ref::<CapReached>().is_some()
}

/// `~/.local/share/nebula/rag.env`; `NEBULA_RAG_ENV` points elsewhere
/// (a temp data dir for tests still uses the real credentials file).
pub fn env_path() -> PathBuf {
    match std::env::var("NEBULA_RAG_ENV") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => nebula_core::paths::data_dir().join("rag.env"),
    }
}

/// KEY=VALUE lines (`export ` and quotes allowed). The process environment
/// wins over the file.
pub fn load_env() -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(env_path()) {
        for line in text.lines() {
            let line = line.trim();
            let line = line.strip_prefix("export ").unwrap_or(line);
            if line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim();
                let v = v
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .or_else(|| v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                    .unwrap_or(v);
                out.insert(k.trim().to_string(), v.to_string());
            }
        }
    }
    for (k, v) in std::env::vars() {
        if k.starts_with("CF_") || k.starts_with("R2_") {
            out.insert(k, v);
        }
    }
    out
}

pub struct WorkersAi {
    account: String,
    token: String,
}

impl WorkersAi {
    pub fn from_env() -> Result<Self> {
        let env = load_env();
        let get = |k: &str| {
            env.get(k)
                .filter(|v| !v.is_empty())
                .cloned()
                .ok_or_else(|| anyhow!("{k} missing — expected in {}", env_path().display()))
        };
        Ok(Self {
            account: get("CF_ACCOUNT_ID")?,
            token: get("CF_AI_TOKEN")?,
        })
    }

    fn request(&self, texts: &[String]) -> Result<Embedded> {
        let url = format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/ai/run/{}",
            self.account,
            crate::db::MODEL
        );
        let body = serde_json::json!({ "text": texts }).to_string();
        let config = format!(
            "url = \"{}\"\nheader = \"Authorization: Bearer {}\"\nheader = \"Content-Type: application/json\"\ndata-binary = \"{}\"\nsilent\nshow-error\nmax-time = 120\nwrite-out = \"\\n%{{http_code}}\"\n",
            curl_escape(&url),
            curl_escape(&self.token),
            curl_escape(&body)
        );
        let mut last = String::new();
        for attempt in 0..4 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_secs(2u64 << attempt));
            }
            let (status, text) = curl(&config)?;
            let json: Option<Value> = serde_json::from_str(&text).ok();
            if status == 200 {
                return parse_response(json.as_ref(), texts.len());
            }
            let detail = json
                .as_ref()
                .and_then(|j| j.get("errors"))
                .map(|e| e.to_string())
                .unwrap_or_else(|| text.chars().take(300).collect());
            // 4006: the account's daily free allocation is used up.
            if detail.contains("4006") || detail.to_lowercase().contains("daily free allocation") {
                return Err(CapReached(format!("Workers AI: {detail}")).into());
            }
            last = format!("Workers AI HTTP {status}: {detail}");
            if !(status == 429 || status >= 500 || status == 0) {
                break;
            }
        }
        bail!(last)
    }
}

impl Embedder for WorkersAi {
    fn embed(&self, texts: &[String]) -> Result<Embedded> {
        let mut all = Embedded {
            vectors: Vec::with_capacity(texts.len()),
            neurons: 0.0,
            tokens: 0,
        };
        for batch in texts.chunks(BATCH) {
            let e = self.request(batch)?;
            all.vectors.extend(e.vectors);
            all.neurons += e.neurons;
            all.tokens += e.tokens;
        }
        Ok(all)
    }
}

fn curl(config: &str) -> Result<(u16, String)> {
    let mut child = Command::new("curl")
        .args(["-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run curl — is it installed?")?;
    child
        .stdin
        .take()
        .context("curl stdin")?
        .write_all(config.as_bytes())?;
    let out = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let (text, code) = stdout.rsplit_once('\n').unwrap_or(("", stdout.as_str()));
    let status = code.trim().parse().unwrap_or(0);
    if status == 0 {
        let err = String::from_utf8_lossy(&out.stderr);
        return Ok((0, err.trim().to_string()));
    }
    Ok((status, text.to_string()))
}

/// Quote for a curl config double-quoted string.
fn curl_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn parse_response(json: Option<&Value>, n: usize) -> Result<Embedded> {
    let json = json.ok_or_else(|| anyhow!("Workers AI: response is not JSON"))?;
    let result = json
        .get("result")
        .ok_or_else(|| anyhow!("Workers AI: no result"))?;
    let data = result
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Workers AI: no result.data"))?;
    let vectors: Vec<Vec<f32>> = data
        .iter()
        .map(|row| {
            row.as_array()
                .map(|r| {
                    r.iter()
                        .filter_map(Value::as_f64)
                        .map(|x| x as f32)
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();
    if vectors.len() != n || vectors.iter().any(|v: &Vec<f32>| v.len() != DIMS) {
        bail!(
            "Workers AI: expected {n}×{DIMS} vectors, got {}",
            vectors.len()
        );
    }
    let meta = result.get("meta");
    let tokens = meta
        .and_then(|m| m.get("cost_metric_value_1"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let neurons = meta
        .and_then(|m| m.get("neurons"))
        .and_then(Value::as_f64)
        .unwrap_or_else(|| estimate_neurons_for_tokens(tokens));
    Ok(Embedded {
        vectors,
        neurons,
        tokens,
    })
}

/// bge-m3 costs ≈ 1,075 neurons per 1M input tokens.
pub fn estimate_neurons_for_tokens(tokens: u64) -> f64 {
    tokens as f64 * 1075.0 / 1_000_000.0
}

/// Pre-flight estimate for a batch, on the high side (≈3 chars/token:
/// Romanian and markdown tokenize denser than English prose).
pub fn estimate_neurons(texts: &[String]) -> f64 {
    let chars: usize = texts.iter().map(|t| t.chars().count()).sum();
    estimate_neurons_for_tokens((chars / 3) as u64 + texts.len() as u64)
}

#[cfg(test)]
pub mod fake {
    //! Deterministic stand-in for tests: a bag of hashed word features, so
    //! texts sharing words land close together and identical texts score 1.

    use super::*;
    use std::cell::Cell;

    #[derive(Default)]
    pub struct FakeEmbedder {
        pub calls: Cell<usize>,
        pub texts: Cell<usize>,
    }

    pub fn vector(text: &str) -> Vec<f32> {
        let mut v = vec![0f32; DIMS];
        for w in text
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
        {
            let h = w.bytes().fold(1469598103934665603u64, |h, b| {
                (h ^ b as u64).wrapping_mul(1099511628211)
            });
            v[(h % DIMS as u64) as usize] += 1.0;
        }
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        v.iter_mut().for_each(|x| *x /= n);
        v
    }

    impl Embedder for FakeEmbedder {
        fn embed(&self, texts: &[String]) -> Result<Embedded> {
            self.calls.set(self.calls.get() + 1);
            self.texts.set(self.texts.get() + texts.len());
            Ok(Embedded {
                vectors: texts.iter().map(|t| vector(t)).collect(),
                neurons: estimate_neurons(texts),
                tokens: texts.iter().map(|t| t.len() as u64 / 4).sum(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_workers_ai_response_with_its_neuron_cost() {
        let row: Vec<f64> = vec![0.5; DIMS];
        let json = serde_json::json!({
            "result": {"data": [row.clone(), row], "shape": [2, DIMS],
                       "meta": {"cost_metric_value_1": 42, "neurons": 0.05}},
            "success": true
        });
        let e = parse_response(Some(&json), 2).unwrap();
        assert_eq!(e.vectors.len(), 2);
        assert_eq!(e.tokens, 42);
        assert!((e.neurons - 0.05).abs() < 1e-9);
        assert!(parse_response(Some(&json), 3).is_err());
    }

    #[test]
    fn curl_config_strings_are_escaped() {
        assert_eq!(curl_escape(r#"{"a":"b\n"}"#), r#"{\"a\":\"b\\n\"}"#);
    }
}
