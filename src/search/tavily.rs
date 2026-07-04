//! Tavily search API client — used for gated web augmentation in /ask.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashSet;

/// A single web search result passed to Tier 5 synthesis.
#[derive(Debug, Clone)]
pub struct WebSnippet {
    pub title: String,
    pub url: String,
    pub content: String,
}

pub struct TavilySearch {
    api_key: String,
    max_results: usize,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct TavilyResponse {
    results: Vec<TavilyResult>,
}

#[derive(Deserialize)]
struct TavilyResult {
    title: String,
    url: String,
    content: String,
}

impl TavilySearch {
    pub fn new(api_key: String, max_results: usize) -> Self {
        Self {
            api_key,
            max_results: max_results.max(1),
            http: reqwest::Client::new(),
        }
    }

    pub async fn search(&self, query: &str) -> Result<Vec<WebSnippet>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(vec![]);
        }

        #[derive(serde::Serialize)]
        struct Body<'a> {
            api_key: &'a str,
            query: &'a str,
            max_results: usize,
            search_depth: &'static str,
            include_answer: bool,
        }

        let body = Body {
            api_key: &self.api_key,
            query,
            max_results: self.max_results,
            search_depth: "basic",
            include_answer: false,
        };

        let resp = self
            .http
            .post("https://api.tavily.com/search")
            .json(&body)
            .send()
            .await
            .context("tavily request")?;

        let status = resp.status();
        let text = resp.text().await.context("tavily response body")?;
        if !status.is_success() {
            anyhow::bail!("tavily HTTP {status}: {text}");
        }

        let parsed: TavilyResponse = serde_json::from_str(&text).context("tavily JSON parse")?;
        Ok(parsed
            .results
            .into_iter()
            .map(|r| WebSnippet {
                title: r.title,
                url: r.url,
                content: r.content,
            })
            .collect())
    }

    /// Run several queries, dedupe by URL, return up to `cap` snippets.
    pub async fn search_many(&self, queries: &[String], cap: usize) -> Result<Vec<WebSnippet>> {
        let cap = cap.max(1);
        let mut seen = HashSet::new();
        let mut out = Vec::new();

        for q in queries {
            if out.len() >= cap {
                break;
            }
            let q = q.trim();
            if q.is_empty() {
                continue;
            }
            let batch = self.search(q).await.unwrap_or_default();
            for s in batch {
                if seen.insert(s.url.clone()) {
                    out.push(s);
                    if out.len() >= cap {
                        break;
                    }
                }
            }
        }
        Ok(out)
    }
}
