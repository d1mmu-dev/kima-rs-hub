//! Apple's podcast catalogue as kima reads it (`podcastCatalog.ts`): the category tree,
//! per-category charts, each show's own categories, and what discovery builds from them.
//! Answers are cached in Postgres (`http_cache`, D-202) with upstream's lifetimes; loads still on
//! their way are shared by later askers, and a person's requests go before background work.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt;
use futures::future::{BoxFuture, Shared, WeakShared};
use kima_db::PgPool;
use kima_db::http_cache::{self, CacheOutcome};
use kima_net::{Host, Net, Req};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::feed::FeedEpisode;
use crate::upstream::{Upstream, classify};

/// Apple's root "Podcasts" genre: every show carries it, so it says nothing.
const ROOT_GENRE: i32 = 26;
/// Apple's chart feed serves at most 200 shows per chart.
pub const CHART_MAX: usize = 200;
const LOOKUP_BATCH: usize = 200;
const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(86_400);
const SEARCH_DEADLINE: Duration = Duration::from_secs(5);

/// Who is waiting (`ApplePriority`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// A person (a show page, the top shows, "for you").
    Interactive,
    /// Work that can wait (the podcasts page's category rows, the category fill-in).
    Background,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    pub id: i32,
    pub name: String,
    pub parent_id: Option<i32>,
}

/// One show on a chart (`CatalogPodcast`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartShow {
    pub itunes_id: String,
    pub title: String,
    pub author: String,
    pub description: String,
    pub cover_url: Option<String>,
    pub category_ids: Vec<i32>,
    pub chart_rank: Option<i32>,
}

/// Apple's record of a show (`LookupShow`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LookupShow {
    pub itunes_id: String,
    pub feed_url: Option<String>,
    pub category_ids: Vec<i32>,
    pub title: String,
    pub author: String,
    pub cover_url: Option<String>,
    pub episode_count: i32,
}

/// A show as the discovery endpoints answer it (`DiscoveryItem`, `podcastCatalog.ts:317-347`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryItem {
    pub id: String,
    pub itunes_id: String,
    pub title: String,
    pub author: String,
    pub description: String,
    pub cover_url: Option<String>,
    pub feed_url: Option<String>,
    pub genres: Vec<String>,
    pub category_ids: Vec<i32>,
    pub episode_count: i32,
    pub chart_rank: Option<i32>,
    pub is_external: bool,
}

/// One "for you" row (`ForYouRow`).
#[derive(Debug, Clone, PartialEq)]
pub struct ForYouRow {
    pub category: Category,
    /// The followed shows that put this category here (at most two).
    pub because: Vec<String>,
    pub podcasts: Vec<DiscoveryItem>,
}

type Load = Shared<BoxFuture<'static, Result<Value, Arc<Upstream>>>>;

struct Flight {
    load: WeakShared<BoxFuture<'static, Result<Value, Arc<Upstream>>>>,
    /// Set once a person waits on this load.
    interactive: Arc<kima_net::gate::Priority>,
}

struct Inner {
    net: Arc<Net>,
    pool: PgPool,
    inflight: Mutex<HashMap<String, Flight>>,
}

/// The catalogue; cheap to clone (one per process, shared).
#[derive(Clone)]
pub struct Catalog(Arc<Inner>);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Ends a flight however its load ends (answered, failed, or dropped unpolled): the entry
/// leaves the map, so a failure is never handed to a later asker; any queued gate waiter
/// is also removed when its future is dropped.
struct Landing {
    inner: Arc<Inner>,
    key: String,
    flag: Arc<kima_net::gate::Priority>,
}

impl Drop for Landing {
    fn drop(&mut self) {
        let mut map = lock(&self.inner.inflight);
        if map
            .get(&self.key)
            .is_some_and(|f| Arc::ptr_eq(&f.interactive, &self.flag))
        {
            map.remove(&self.key);
        }
    }
}

/// JavaScript truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn has_feed(v: &Value) -> bool {
    truthy(v.get("feed"))
}

fn has_results(v: &Value) -> bool {
    v.get("results").is_some_and(Value::is_array)
}

fn has_categories(v: &Value) -> bool {
    !category_tree(v).is_empty()
}

/// JavaScript `String(x)` of an id: a string as is, a number in decimal.
fn id_string(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// `Number(x)` when it is an integer.
fn integer(v: &Value) -> Option<i32> {
    let n = match v {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) if s.trim().is_empty() => 0.0,
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    (n.fract() == 0.0 && n.abs() <= f64::from(i32::MAX)).then_some(n as i32)
}

fn label(v: Option<&Value>) -> String {
    v.and_then(|l| l.get("label"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Apple's genre ids for a show, most specific first, without the root (`categoryIdsFrom`).
pub fn category_ids_from(genre_ids: Option<&Value>) -> Vec<i32> {
    genre_ids
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(integer)
                .filter(|&g| g != ROOT_GENRE)
                .collect()
        })
        .unwrap_or_default()
}

/// `/\d+x\d+bb\./` → `/600x600bb.` (the first match), so a page shows the cover sharply.
fn large_artwork(url: Option<&str>) -> Option<String> {
    let url = url.filter(|u| !u.is_empty())?;
    let bytes = url.as_bytes();
    for start in url.match_indices('/').map(|(i, _)| i) {
        let rest = &url[start + 1..];
        let w = rest.bytes().take_while(u8::is_ascii_digit).count();
        if w == 0 || rest.as_bytes().get(w) != Some(&b'x') {
            continue;
        }
        let after = &rest[w + 1..];
        let h = after.bytes().take_while(u8::is_ascii_digit).count();
        if h > 0 && after[h..].starts_with("bb.") {
            let end = start + 1 + w + 1 + h + 3;
            debug_assert!(end <= bytes.len());
            return Some(format!("{}/600x600bb.{}", &url[..start], &url[end..]));
        }
    }
    Some(url.to_string())
}

/// JavaScript object keys in iteration order: integer-like keys ascending, then the rest.
fn js_keys(map: &serde_json::Map<String, Value>) -> Vec<&String> {
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort_by_key(|k| match k.parse::<u32>() {
        Ok(n) if n.to_string() == **k => (0, n),
        _ => (1, 0),
    });
    keys
}

/// The flattened tree under the root genre (`categoryTree`, `podcastCatalog.ts:141-155`).
fn category_tree(data: &Value) -> Vec<Category> {
    let mut out = Vec::new();
    let Some(tops) = data
        .get(ROOT_GENRE.to_string())
        .and_then(|r| r.get("subgenres"))
        .and_then(Value::as_object)
    else {
        return out;
    };
    for id in js_keys(tops) {
        let top = &tops[id];
        let Ok(top_id) = id.parse::<i32>() else {
            continue;
        };
        out.push(Category {
            id: top_id,
            name: top.get("name").and_then(Value::as_str).unwrap_or("").into(),
            parent_id: None,
        });
        if let Some(subs) = top.get("subgenres").and_then(Value::as_object) {
            for sub_id in js_keys(subs) {
                let Ok(n) = sub_id.parse::<i32>() else {
                    continue;
                };
                out.push(Category {
                    id: n,
                    name: subs[sub_id]
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into(),
                    parent_id: Some(top_id),
                });
            }
        }
    }
    out
}

/// A chart reply's shows in rank order (`podcastCatalog.ts:163-182`).
fn chart_shows(data: &Value) -> Vec<ChartShow> {
    let entry = data.get("feed").and_then(|f| f.get("entry"));
    let entries: Vec<&Value> = match entry {
        Some(Value::Array(list)) => list.iter().collect(),
        Some(e) if truthy(Some(e)) => vec![e],
        _ => Vec::new(),
    };
    entries
        .into_iter()
        .enumerate()
        .map(|(i, e)| {
            let images = e.get("im:image").and_then(Value::as_array);
            let category = e
                .get("category")
                .and_then(|c| c.get("attributes"))
                .and_then(|a| a.get("im:id"))
                .and_then(integer)
                .filter(|&c| c != ROOT_GENRE);
            ChartShow {
                itunes_id: id_string(
                    e.get("id")
                        .and_then(|i| i.get("attributes"))
                        .and_then(|a| a.get("im:id")),
                ),
                title: label(e.get("im:name")),
                author: label(e.get("im:artist")),
                description: label(e.get("summary")),
                cover_url: large_artwork(
                    images
                        .and_then(|i| i.last())
                        .and_then(|i| i.get("label"))
                        .and_then(Value::as_str),
                ),
                category_ids: category.into_iter().collect(),
                chart_rank: Some(i as i32 + 1),
            }
        })
        .filter(|p| !p.itunes_id.is_empty())
        .collect()
}

fn lookup_show(r: &Value) -> LookupShow {
    let text = |k: &str| r.get(k).and_then(Value::as_str).map(str::to_string);
    LookupShow {
        itunes_id: id_string(r.get("collectionId")),
        feed_url: text("feedUrl"),
        category_ids: category_ids_from(r.get("genreIds")),
        title: text("collectionName").unwrap_or_default(),
        author: text("artistName").unwrap_or_default(),
        cover_url: text("artworkUrl600")
            .filter(|s| !s.is_empty())
            .or_else(|| text("artworkUrl100").filter(|s| !s.is_empty())),
        episode_count: r
            .get("trackCount")
            .and_then(Value::as_i64)
            .and_then(|n| i32::try_from(n).ok())
            .unwrap_or(0),
    }
}

fn lookup_key(id: &str) -> String {
    format!("apple:lookup:v3:{id}")
}

impl Catalog {
    pub fn new(net: Arc<Net>, pool: PgPool) -> Catalog {
        Catalog(Arc::new(Inner {
            net,
            pool,
            inflight: Mutex::new(HashMap::new()),
        }))
    }

    /// One Apple JSON request through the cache, shared with anyone already waiting for the same
    /// `key` (`cached`, `podcastCatalog.ts:84-120`).
    async fn load(&self, key: String, req: Req, priority: Priority) -> Result<Value, Upstream> {
        let interactive = priority == Priority::Interactive;
        let load = {
            let mut map = lock(&self.0.inflight);
            match map.get(&key).and_then(|flight| {
                flight
                    .load
                    .upgrade()
                    .map(|load| (load, &flight.interactive))
            }) {
                Some((load, flag)) => {
                    if interactive {
                        flag.raise();
                    }
                    load
                }
                None => {
                    let flag = Arc::new(if interactive {
                        kima_net::gate::Priority::interactive()
                    } else {
                        kima_net::gate::Priority::background()
                    });
                    let landing = Landing {
                        inner: self.0.clone(),
                        key: key.clone(),
                        flag: flag.clone(),
                    };
                    let inner = self.0.clone();
                    let load: Load = async move {
                        let landing = landing;
                        classify(
                            inner
                                .net
                                .get_json_with_priority::<Value>(req, &landing.flag)
                                .await,
                        )
                        .map_err(Arc::new)
                    }
                    .boxed()
                    .shared();
                    map.insert(
                        key,
                        Flight {
                            load: load.downgrade().expect("new shared future has an owner"),
                            interactive: flag,
                        },
                    );
                    load
                }
            }
        };
        load.await.map_err(|e| e.copy())
    }

    fn gate_priority(priority: Priority) -> kima_net::gate::Priority {
        match priority {
            Priority::Interactive => kima_net::gate::Priority::interactive(),
            Priority::Background => kima_net::gate::Priority::background(),
        }
    }

    /// `chartCountry` (`podcastCatalog.ts:134-138`): the setting, lowercase, if it is two
    /// letters; else `us`.
    pub async fn chart_country(&self) -> String {
        let stored = kima_db::podcasts::chart_country(&self.0.pool)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "could not read podcastChartCountry");
                None
            });
        let cc = stored.unwrap_or_else(|| "us".into()).to_lowercase();
        if cc.len() == 2 && cc.bytes().all(|b| b.is_ascii_lowercase()) {
            cc
        } else {
            "us".into()
        }
    }

    /// Every podcast category Apple has, top-level and sub-categories, flattened (7 days).
    pub async fn categories(&self) -> Result<Vec<Category>, Upstream> {
        let req = Req::get(Host::Itunes, "/WebObjects/MZStoreServices.woa/ws/genres")
            .query("id", ROOT_GENRE.to_string())
            .ttl(7 * DAY, 7 * DAY)
            .valid_when(has_categories)
            .user_facing();
        let data = self
            .load("categories:v1".into(), req, Priority::Interactive)
            .await?;
        Ok(category_tree(&data))
    }

    /// Apple's chart for one category (the overall chart without one), in rank order, up to
    /// 200 shows (6 hours).
    pub async fn chart(
        &self,
        category: Option<i32>,
        priority: Priority,
    ) -> Result<Vec<ChartShow>, Upstream> {
        let country = self.chart_country().await;
        let genre = category.map_or(String::new(), |c| format!("/genre={c}"));
        let path = format!("/{country}/rss/toppodcasts/limit={CHART_MAX}{genre}/json");
        let req = Req::get(Host::Itunes, &path)
            .ttl(6 * HOUR, 6 * HOUR)
            .valid_when(has_feed)
            .user_facing();
        let key = format!(
            "chart:v1:{country}:{}",
            category.map_or("all".into(), |c| c.to_string())
        );
        Ok(chart_shows(&self.load(key, req, priority).await?))
    }

    /// Apple's podcast search for a term, as Apple answers it (`searchShows`): 5 s in all,
    /// queue included, and a rate-limited search fails at once.
    pub async fn search(&self, term: &str, limit: u32) -> Result<Vec<Value>, Upstream> {
        let req = Req::get(Host::Itunes, "/search")
            .query("term", term)
            .query("media", "podcast")
            .query("entity", "podcast")
            .query("limit", limit.to_string())
            .no_cache()
            .no_retry()
            .budget(SEARCH_DEADLINE)
            .user_facing();
        let gate_priority = Self::gate_priority(Priority::Interactive);
        let asked = self
            .0
            .net
            .get_json_with_priority::<Value>(req, &gate_priority);
        let out = tokio::time::timeout(SEARCH_DEADLINE, asked)
            .await
            .map_err(|_| Upstream::Down("Apple podcast search took longer than 5 s".into()))?;
        let data = classify(out)?;
        Ok(data
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Apple's record for each show id (`lookupShows`): ids that are not all digits are
    /// dropped; each id is remembered 7 days, an id Apple does not know as unknown.
    pub async fn lookup(
        &self,
        ids: &[String],
        priority: Priority,
    ) -> Result<HashMap<String, LookupShow>, Upstream> {
        let pool = &self.0.pool;
        let mut out = HashMap::new();
        let mut seen = HashSet::new();
        let mut missing = Vec::new();
        for id in ids {
            if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) || !seen.insert(id) {
                continue;
            }
            match http_cache::get(pool, &lookup_key(id)).await {
                Ok(Some((CacheOutcome::Answered, body))) => {
                    if let Ok(show) = serde_json::from_value::<LookupShow>(body) {
                        out.insert(id.clone(), show);
                        continue;
                    }
                    missing.push(id.clone());
                }
                Ok(Some((CacheOutcome::Absent, _))) => {}
                Ok(None) => missing.push(id.clone()),
                Err(e) => {
                    tracing::warn!(error = %e, "could not read the Apple lookup cache");
                    missing.push(id.clone());
                }
            }
        }
        for batch in missing.chunks(LOOKUP_BATCH) {
            let req = Req::get(Host::Itunes, "/lookup")
                .query("id", batch.join(","))
                .query("entity", "podcast")
                .no_cache()
                .valid_when(has_results)
                .user_facing();
            let data = classify(
                self.0
                    .net
                    .get_json_with_priority::<Value>(req, &Self::gate_priority(priority))
                    .await,
            )?;
            let mut found: HashMap<String, LookupShow> = HashMap::new();
            for r in data["results"].as_array().into_iter().flatten() {
                if r.get("kind").and_then(Value::as_str) != Some("podcast") {
                    continue;
                }
                let show = lookup_show(r);
                found.insert(show.itunes_id.clone(), show);
            }
            for id in batch {
                let show = found.remove(id);
                let (outcome, body) = match &show {
                    Some(s) => (
                        CacheOutcome::Answered,
                        serde_json::to_value(s).unwrap_or(Value::Null),
                    ),
                    None => (CacheOutcome::Absent, Value::Null),
                };
                let (key, request) = (lookup_key(id), format!("lookup {id}"));
                let put = http_cache::put(pool, &key, "apple", &request, outcome, &body, 7 * DAY);
                if let Err(e) = put.await {
                    tracing::warn!(error = %e, "could not write the Apple lookup cache");
                }
                if let Some(show) = show {
                    out.insert(id.clone(), show);
                }
            }
        }
        Ok(out)
    }

    /// A feed's own `itunes:category` names mapped onto Apple's ids, case-insensitively; names
    /// Apple does not use are dropped (`categoryIdsForNames`).
    pub async fn category_ids_for_names(&self, names: &[String]) -> Result<Vec<i32>, Upstream> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let by_name: HashMap<String, i32> = self
            .categories()
            .await?
            .into_iter()
            .map(|c| (c.name.to_lowercase(), c.id))
            .collect();
        let mut ids = Vec::new();
        for name in names {
            if let Some(&id) = by_name.get(&name.to_lowercase()) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        Ok(ids)
    }

    /// Resolve Apple's categories or fall back to feed names, without a database transaction.
    pub async fn resolve_categories(
        &self,
        itunes: Option<&str>,
        names: &[String],
        priority: Priority,
    ) -> Result<Vec<i32>, Upstream> {
        let mut ids = Vec::new();
        if let Some(itunes) = itunes {
            ids = self
                .lookup(&[itunes.to_string()], priority)
                .await?
                .remove(itunes)
                .map(|s| s.category_ids)
                .unwrap_or_default();
        }
        if ids.is_empty() {
            ids = self.category_ids_for_names(names).await?;
        }
        Ok(ids)
    }

    /// Stores a show's categories: Apple's record when it has an Apple id, else its feed's own
    /// names (`assignCategories`). Never fails; a show Apple cannot answer for keeps what it had.
    pub async fn assign_categories(
        &self,
        show: Uuid,
        itunes: Option<&str>,
        names: &[String],
        priority: Priority,
    ) -> Vec<i32> {
        let assigned: Result<Vec<i32>, Upstream> = async {
            let ids = self.resolve_categories(itunes, names, priority).await?;
            if !ids.is_empty() {
                kima_db::podcasts::set_category_ids(&self.0.pool, show, &ids)
                    .await
                    .map_err(Upstream::internal)?;
            }
            Ok(ids)
        }
        .await;
        assigned.unwrap_or_else(|e| {
            tracing::warn!(%show, error = %e, "could not assign podcast categories");
            Vec::new()
        })
    }

    /// Shows with an Apple id and no categories get the ones Apple names, after one batched
    /// lookup (`assignMissingCategories`). Returns how many were filled.
    pub async fn assign_missing_categories(&self) -> usize {
        let shows = match kima_db::podcasts::missing_categories(&self.0.pool).await {
            Ok(shows) => shows,
            Err(e) => {
                tracing::warn!(error = %e, "could not list shows without categories");
                return 0;
            }
        };
        let ids: Vec<String> = shows.iter().map(|(_, it)| it.clone()).collect();
        if let Err(e) = self.lookup(&ids, Priority::Background).await {
            tracing::warn!(error = %e, "batched lookup for the category backfill failed");
        }
        let mut filled = 0;
        for (id, itunes) in &shows {
            if !self
                .assign_categories(*id, Some(itunes), &[], Priority::Background)
                .await
                .is_empty()
            {
                filled += 1;
            }
        }
        if filled > 0 {
            tracing::info!(filled, "podcast categories filled");
        }
        filled
    }

    /// Chart shows as discovery items, category names from Apple's tree (`toDiscoveryItems`).
    pub async fn to_discovery_items(
        &self,
        shows: Vec<ChartShow>,
    ) -> Result<Vec<DiscoveryItem>, Upstream> {
        let names: HashMap<i32, String> = self
            .categories()
            .await?
            .into_iter()
            .map(|c| (c.id, c.name))
            .collect();
        Ok(shows
            .into_iter()
            .map(|p| DiscoveryItem {
                id: p.itunes_id.clone(),
                genres: p
                    .category_ids
                    .iter()
                    .filter_map(|id| names.get(id).filter(|n| !n.is_empty()).cloned())
                    .collect(),
                itunes_id: p.itunes_id,
                title: p.title,
                author: p.author,
                description: p.description,
                cover_url: p.cover_url,
                feed_url: None,
                category_ids: p.category_ids,
                episode_count: 0,
                chart_rank: p.chart_rank,
                is_external: true,
            })
            .collect())
    }

    /// Rows from what `user` follows (`forYou`, `podcastCatalog.ts:360-387`): the categories of
    /// their shows weighed `(sub-category ? 2 : 1) / (position + 1)`, the top three, each that
    /// category's chart without shows already followed, twelve a row.
    pub async fn for_you(&self, user: Uuid) -> Result<Vec<ForYouRow>, Upstream> {
        const ROWS: usize = 3;
        const PER_ROW: usize = 12;
        let subs = kima_db::podcasts::followed(&self.0.pool, user)
            .await
            .map_err(Upstream::internal)?;
        let tree = self.categories().await?;
        let by_id: HashMap<i32, &Category> = tree.iter().map(|c| (c.id, c)).collect();
        // Insertion order is kept for ties, as a JavaScript Map's.
        let mut weight: Vec<(i32, f64, Vec<String>)> = Vec::new();
        for show in &subs {
            for (position, id) in show.category_ids.iter().enumerate() {
                let Some(category) = by_id.get(id) else {
                    continue;
                };
                let w = if category.parent_id.is_some() {
                    2.0
                } else {
                    1.0
                };
                let add = w / (position as f64 + 1.0);
                match weight.iter_mut().find(|(c, _, _)| c == id) {
                    Some((_, score, because)) => {
                        *score += add;
                        because.push(show.title.clone());
                    }
                    None => weight.push((*id, add, vec![show.title.clone()])),
                }
            }
        }
        let followed: HashSet<&str> = subs.iter().filter_map(|s| s.itunes_id.as_deref()).collect();
        weight.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut out = Vec::new();
        for (id, _, because) in weight.into_iter().take(ROWS) {
            let shows: Vec<ChartShow> = self
                .chart(Some(id), Priority::Interactive)
                .await?
                .into_iter()
                .filter(|p| !followed.contains(p.itunes_id.as_str()))
                .take(PER_ROW)
                .collect();
            if shows.is_empty() {
                continue;
            }
            let mut unique: Vec<String> = Vec::new();
            for title in because {
                if !unique.contains(&title) {
                    unique.push(title);
                }
            }
            unique.truncate(2);
            out.push(ForYouRow {
                category: by_id[&id].clone(),
                because: unique,
                podcasts: self.to_discovery_items(shows).await?,
            });
        }
        Ok(out)
    }

    /// Shows like this one (`similarShows`): those on its categories' charts, closest category
    /// list first (shared over all, from Apple's full list for each show), then chart order;
    /// twelve.
    pub async fn similar(
        &self,
        itunes: Option<&str>,
        category_ids: &[i32],
    ) -> Result<Vec<DiscoveryItem>, Upstream> {
        const LIMIT: usize = 12;
        let target: HashSet<i32> = category_ids.iter().copied().collect();
        let mut seen = HashSet::new();
        let mut candidates: Vec<ChartShow> = Vec::new();
        for id in category_ids {
            for p in self.chart(Some(*id), Priority::Interactive).await? {
                if Some(p.itunes_id.as_str()) == itunes || !seen.insert(p.itunes_id.clone()) {
                    continue;
                }
                candidates.push(p);
            }
        }
        let ids: Vec<String> = candidates.iter().map(|p| p.itunes_id.clone()).collect();
        let looked = self.lookup(&ids, Priority::Interactive).await?;
        let closeness = |p: &ChartShow| {
            let own: HashSet<i32> = looked
                .get(&p.itunes_id)
                .map_or(&p.category_ids, |s| &s.category_ids)
                .iter()
                .copied()
                .collect();
            let shared = own.intersection(&target).count();
            let all = own.union(&target).count();
            if all == 0 {
                0.0
            } else {
                shared as f64 / all as f64
            }
        };
        let mut ranked: Vec<(usize, f64, ChartShow)> = candidates
            .into_iter()
            .enumerate()
            .map(|(order, p)| (order, closeness(&p), p))
            .collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let top = ranked.into_iter().take(LIMIT).map(|(_, _, p)| p).collect();
        self.to_discovery_items(top).await
    }
}

/// The oldest episode with audio in a parsed feed, where hosts usually say what the show is
/// (`oldestEpisode`); the first of equally old ones.
pub fn oldest_episode(episodes: &[FeedEpisode]) -> Option<&FeedEpisode> {
    let mut oldest: Option<&FeedEpisode> = None;
    for ep in episodes {
        if ep.audio_url.is_empty() {
            continue;
        }
        if oldest.is_none_or(|o| ep.published_at < o.published_at) {
            oldest = Some(ep);
        }
    }
    oldest
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn large_artwork_replaces_the_size() {
        assert_eq!(
            large_artwork(Some("https://is1.example/a/170x170bb.png")).as_deref(),
            Some("https://is1.example/a/600x600bb.png")
        );
        assert_eq!(
            large_artwork(Some("https://is1.example/a/b.png")).as_deref(),
            Some("https://is1.example/a/b.png")
        );
        assert_eq!(large_artwork(Some("")), None);
        assert_eq!(large_artwork(None), None);
    }

    #[test]
    fn category_ids_from_drops_the_root_and_non_integers() {
        assert_eq!(
            category_ids_from(Some(&json!(["1495", "26", 1303, "x", 1.5]))),
            vec![1495, 1303]
        );
        assert!(category_ids_from(Some(&json!("1495"))).is_empty());
        assert!(category_ids_from(None).is_empty());
    }

    #[test]
    fn js_keys_are_numeric_first() {
        let v = json!({"1310": 1, "26": 2, "b": 3, "1301": 4});
        let keys: Vec<&str> = js_keys(v.as_object().unwrap())
            .into_iter()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["26", "1301", "1310", "b"]);
    }
}
