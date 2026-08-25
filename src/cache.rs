use anyhow::Context;
use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sqlite_cache::{Cache, CacheConfig};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub static CACHE: Lazy<ArcSwap<Cache>> =
    Lazy::new(|| open_cache().expect("failed to initialize cache").into());

fn cache_file_name() -> PathBuf {
    let cache_dir = std::env::var("GOVEE_CACHE_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(dirs_next::cache_dir)
        .expect("failed to resolve cache dir");

    cache_dir.join("govee2mqtt-cache.sqlite")
}

fn open_cache() -> anyhow::Result<Arc<Cache>> {
    let cache_file = cache_file_name();
    let conn = sqlite_cache::rusqlite::Connection::open(&cache_file)
        .unwrap_or_else(|_| panic!("failed to open {cache_file:?}"));
    Ok(Arc::new(Cache::new(
        // We have low cardinality and can be pretty relaxed
        CacheConfig {
            flush_gc_ratio: 1024,
            flush_interval: Duration::from_secs(900),
            max_ttl: None,
        },
        conn,
    )?))
}

pub fn purge_cache() -> anyhow::Result<()> {
    let cache_file = cache_file_name();
    std::fs::remove_file(&cache_file)
        .with_context(|| format!("removing cache file {cache_file:?}"))?;
    CACHE.store(open_cache()?);
    Ok(())
}

#[derive(Deserialize, Serialize, Debug)]
struct CacheEntry<T> {
    expires: DateTime<Utc>,
    result: CacheResult<T>,
}

#[derive(Deserialize, Serialize, Debug)]
enum CacheResult<T> {
    Ok(T),
    Err(String),
}

const CACHED_ERROR_MESSAGE: &str =
    "request failure is temporarily cached; retry after the negative-cache window";

impl<T> CacheResult<T> {
    fn into_result(self) -> anyhow::Result<T> {
        match self {
            Self::Ok(v) => Ok(v),
            // Older cache entries may contain the original error text. Never
            // replay that serialized value: it predates the cache-safe error
            // boundary and may contain credentials or response tokens.
            Self::Err(_) => anyhow::bail!(CACHED_ERROR_MESSAGE),
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct CacheGetOptions<'a> {
    pub key: &'a str,
    pub topic: &'a str,
    pub soft_ttl: Duration,
    pub hard_ttl: Duration,
    pub negative_ttl: Duration,
    pub allow_stale: bool,
}

pub enum CacheComputeResult<T> {
    Value(T),
    WithTtl(T, Duration),
}

impl<T> CacheComputeResult<T> {
    #[allow(dead_code)]
    pub fn into_inner(self) -> T {
        match self {
            Self::Value(v) | Self::WithTtl(v, _) => v,
        }
    }
}

pub fn invalidate_key(topic: &str, key: &str) -> anyhow::Result<()> {
    let topic = CACHE.load().topic(topic)?;
    Ok(topic.delete(key)?)
}

/// Marker error: when a cached future returns `Err(anyhow::Error::from(NoCacheError(_)))`,
/// `cache_get` skips the negative-cache write entirely. Use for transient errors where
/// the caller must be able to retry sooner than `negative_ttl` (e.g. 2FA verification
/// codes that expire in ~15 minutes — caching the failure for 15 min would trap the user
/// in a loop matching the code's own validity window).
#[derive(Debug)]
pub struct NoCacheError(pub anyhow::Error);

impl std::fmt::Display for NoCacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for NoCacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

/// Cache an item with a soft TTL; we'll retry the operation
/// if the TTL has expired, but allow stale reads.
///
/// Thin wrapper around [`cache_get_inner`] that uses the global [`CACHE`].
/// Tests use the inner directly with a fresh in-memory cache to stay hermetic.
pub async fn cache_get<T, Fut>(options: CacheGetOptions<'_>, future: Fut) -> anyhow::Result<T>
where
    T: Serialize + DeserializeOwned + std::fmt::Debug + Clone,
    Fut: Future<Output = anyhow::Result<CacheComputeResult<T>>>,
{
    cache_get_inner(&CACHE.load(), options, future).await
}

/// Inner cache_get that takes an explicit `&Cache`. Exposed at crate visibility
/// so unit tests can construct hermetic caches and verify the negative-cache
/// bypass without touching the global singleton.
pub(crate) async fn cache_get_inner<T, Fut>(
    cache: &Cache,
    options: CacheGetOptions<'_>,
    future: Fut,
) -> anyhow::Result<T>
where
    T: Serialize + DeserializeOwned + std::fmt::Debug + Clone,
    Fut: Future<Output = anyhow::Result<CacheComputeResult<T>>>,
{
    let topic = cache.topic(options.topic)?;
    let (updater, current_value) = topic.get_for_update(options.key).await?;
    let now = Utc::now();

    let mut cache_entry: Option<CacheEntry<T>> = None;

    if let Some(current) = &current_value {
        match serde_json::from_slice::<CacheEntry<T>>(&current.data) {
            Ok(entry) => {
                let is_legacy_error = matches!(
                    &entry.result,
                    CacheResult::Err(message) if message != CACHED_ERROR_MESSAGE
                );
                if is_legacy_error {
                    // A pre-fix error may contain credentials or response
                    // tokens and may also have a longer negative TTL. Remove
                    // it before recomputing so neither the bytes nor the old
                    // suppression window survive this cache access.
                    topic.delete(options.key)?;
                } else if now < entry.expires {
                    log::trace!("cache hit for {}", options.key);
                    return entry.result.into_result();
                } else {
                    cache_entry.replace(entry);
                }
            }
            Err(err) => {
                log::warn!(
                    "{}",
                    cache_parse_failure_message::<T>(&options, &err, current.data.len())
                );
                // The payload is unusable and may predate the cache-safe
                // error format. Delete it before recomputing so a transient
                // NoCacheError cannot leave secret-bearing bytes behind.
                topic.delete(options.key)?;
            }
        }
    }

    log::trace!("cache miss for {}", options.key);
    let value: anyhow::Result<CacheComputeResult<T>> = future.await;
    match value {
        Ok(CacheComputeResult::WithTtl(value, ttl)) => {
            let entry = CacheEntry {
                expires: Utc::now() + ttl,
                result: CacheResult::Ok(value.clone()),
            };

            let data = serde_json::to_string_pretty(&entry)?;
            updater.write(data.as_bytes(), options.hard_ttl)?;
            Ok(value)
        }
        Ok(CacheComputeResult::Value(value)) => {
            let entry = CacheEntry {
                expires: Utc::now() + options.soft_ttl,
                result: CacheResult::Ok(value.clone()),
            };

            let data = serde_json::to_string_pretty(&entry)?;
            updater.write(data.as_bytes(), options.hard_ttl)?;
            Ok(value)
        }
        Err(err) => {
            // Opt-out: callers can return Err(NoCacheError(...).into()) to bypass
            // both the stale-read path and the negative-cache write. The next call
            // will re-execute the future instead of returning the cached failure.
            if err.downcast_ref::<NoCacheError>().is_some() {
                log::trace!(
                    "cache_get: NoCacheError marker, skipping negative cache write for {}",
                    options.key
                );
                return Err(err);
            }
            match cache_entry.take() {
                Some(mut entry)
                    if options.allow_stale && matches!(&entry.result, CacheResult::Ok(_)) =>
                {
                    entry.expires = Utc::now() + options.negative_ttl;

                    log::warn!("{err:#}, will use prior results");

                    let data = serde_json::to_string_pretty(&entry)?;
                    updater.write(data.as_bytes(), options.hard_ttl)?;

                    entry.result.into_result()
                }
                _ => {
                    let entry: CacheEntry<T> = CacheEntry {
                        expires: Utc::now() + options.negative_ttl,
                        // Arbitrary errors are not safe cache data. Return the
                        // detailed error to the immediate caller, but persist
                        // only an opaque marker for subsequent cache hits.
                        result: CacheResult::Err(CACHED_ERROR_MESSAGE.to_string()),
                    };

                    let data = serde_json::to_string_pretty(&entry)?;
                    updater.write(data.as_bytes(), options.hard_ttl)?;
                    Err(err)
                }
            }
        }
    }
}

fn cache_parse_failure_message<T>(
    options: &CacheGetOptions<'_>,
    error: &serde_json::Error,
    body_len: usize,
) -> String {
    format!(
        "Error parsing cached {} for topic {:?}, key {:?}: JSON {:?} error at line {}, column {}. \
         Cached payload withheld ({} bytes)",
        std::any::type_name::<CacheEntry<T>>(),
        options.topic,
        options.key,
        error.classify(),
        error.line(),
        error.column(),
        body_len
    )
}

#[cfg(test)]
mod test {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Fresh in-memory SQLite-backed cache for each test. No global state, no
    /// cross-test pollution, no risk of writing to the user's real cache file.
    fn fresh_cache() -> Cache {
        let conn =
            sqlite_cache::rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        Cache::new(
            CacheConfig {
                flush_gc_ratio: 1024,
                flush_interval: Duration::from_secs(900),
                max_ttl: None,
            },
            conn,
        )
        .expect("Cache::new")
    }

    fn opts(key: &'static str, negative_ttl: Duration) -> CacheGetOptions<'static> {
        CacheGetOptions {
            topic: "test",
            key,
            soft_ttl: Duration::from_secs(60),
            hard_ttl: Duration::from_secs(60),
            negative_ttl,
            allow_stale: false,
        }
    }

    #[test]
    fn corrupt_cache_diagnostic_withholds_serialized_value() {
        let body = br#"{
            "expires": "2999-01-01T00:00:00Z",
            "result": {"Ok": "TOKEN_SENTINEL"}
        }"#;
        let error = serde_json::from_slice::<CacheEntry<u64>>(body)
            .expect_err("string cache value must not deserialize as u64");
        let options = opts("account-info", Duration::from_secs(10));
        let rendered = cache_parse_failure_message::<u64>(&options, &error, body.len());

        assert!(!rendered.contains("TOKEN_SENTINEL"), "{rendered}");
        assert!(rendered.contains("topic \"test\""), "{rendered}");
        assert!(rendered.contains("key \"account-info\""), "{rendered}");
        assert!(rendered.contains("JSON Data error"), "{rendered}");
        assert!(rendered.contains("line "), "{rendered}");
        assert!(rendered.contains("column "), "{rendered}");
        assert!(
            rendered.contains(&format!("Cached payload withheld ({} bytes)", body.len())),
            "{rendered}"
        );
    }

    #[tokio::test]
    async fn caches_ok_value_so_future_runs_only_once() {
        let cache = fresh_cache();
        let calls = AtomicUsize::new(0);

        let f1 = async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, anyhow::Error>(CacheComputeResult::Value(42_u64))
        };
        let v1 = cache_get_inner(&cache, opts("ok", Duration::from_secs(10)), f1)
            .await
            .unwrap();

        let f2 = async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, anyhow::Error>(CacheComputeResult::Value(42_u64))
        };
        let v2 = cache_get_inner(&cache, opts("ok", Duration::from_secs(10)), f2)
            .await
            .unwrap();

        assert_eq!(v1, 42);
        assert_eq!(v2, 42);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "second call should hit cache, not re-execute"
        );
    }

    #[tokio::test]
    async fn plain_err_writes_negative_cache_so_future_runs_only_once() {
        let cache = fresh_cache();
        let calls = AtomicUsize::new(0);

        let f1 = async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err::<CacheComputeResult<u64>, _>(anyhow::anyhow!("boom"))
        };
        let r1 = cache_get_inner(&cache, opts("err", Duration::from_secs(60)), f1).await;

        let f2 = async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err::<CacheComputeResult<u64>, _>(anyhow::anyhow!("boom"))
        };
        let r2 = cache_get_inner(&cache, opts("err", Duration::from_secs(60)), f2).await;

        assert!(r1.is_err());
        assert!(r2.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "plain Err must be negative-cached; future must NOT re-execute"
        );
    }

    #[tokio::test]
    async fn negative_cache_never_persists_or_replays_error_details() {
        const SECRET: &str = "OPTED_IN_RESPONSE_SECRET_SENTINEL";

        let cache = fresh_cache();
        let calls = AtomicUsize::new(0);

        let first = cache_get_inner(
            &cache,
            opts("secret-error", Duration::from_secs(60)),
            async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<CacheComputeResult<u64>, _>(anyhow::anyhow!(SECRET))
            },
        )
        .await
        .expect_err("the immediate request must return its detailed failure");
        assert!(format!("{first:#}").contains(SECRET), "{first:#}");

        let stored = cache
            .topic("test")
            .expect("open test topic")
            .get("secret-error")
            .expect("read cached error")
            .expect("negative-cache entry must exist");
        let stored = String::from_utf8(stored.data).expect("cache entry must be UTF-8 JSON");
        assert!(
            !stored.contains(SECRET),
            "secret persisted in cache: {stored}"
        );
        assert!(stored.contains(CACHED_ERROR_MESSAGE), "{stored}");

        let second = cache_get_inner(
            &cache,
            opts("secret-error", Duration::from_secs(60)),
            async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<CacheComputeResult<u64>, _>(anyhow::anyhow!(
                    "future must not execute during the negative-cache window"
                ))
            },
        )
        .await
        .expect_err("the cached failure must remain an error");
        let rendered = format!("{second:#}");

        assert!(
            !rendered.contains(SECRET),
            "cached error replayed secret: {rendered}"
        );
        assert!(rendered.contains(CACHED_ERROR_MESSAGE), "{rendered}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "negative-cache hit must not execute the second future"
        );
    }

    #[tokio::test]
    async fn failed_refresh_reuses_only_a_stale_success() {
        let cache = fresh_cache();
        let options = CacheGetOptions {
            topic: "test",
            key: "stale-success",
            soft_ttl: Duration::ZERO,
            hard_ttl: Duration::from_secs(60),
            negative_ttl: Duration::from_secs(60),
            allow_stale: true,
        };

        let first = cache_get_inner(&cache, options, async {
            Ok::<_, anyhow::Error>(CacheComputeResult::Value(42_u64))
        })
        .await
        .expect("seed stale successful value");
        let second = cache_get_inner(&cache, options, async {
            Err::<CacheComputeResult<u64>, _>(anyhow::anyhow!("refresh failed"))
        })
        .await
        .expect("failed refresh must return the stale success");

        assert_eq!(first, 42);
        assert_eq!(second, 42);
    }

    #[tokio::test]
    async fn expired_cached_error_returns_the_current_failure() {
        let cache = fresh_cache();
        let options = CacheGetOptions {
            topic: "test",
            key: "expired-error",
            soft_ttl: Duration::ZERO,
            hard_ttl: Duration::from_secs(60),
            negative_ttl: Duration::ZERO,
            allow_stale: true,
        };

        cache_get_inner(&cache, options, async {
            Err::<CacheComputeResult<u64>, _>(anyhow::anyhow!("first failure"))
        })
        .await
        .expect_err("seed expired negative-cache entry");

        let second = cache_get_inner(&cache, options, async {
            Err::<CacheComputeResult<u64>, _>(anyhow::anyhow!("CURRENT_FAILURE_SENTINEL"))
        })
        .await
        .expect_err("expired cached error must execute and return the current failure");
        let rendered = format!("{second:#}");

        assert!(rendered.contains("CURRENT_FAILURE_SENTINEL"), "{rendered}");
        assert!(!rendered.contains("first failure"), "{rendered}");
    }

    #[tokio::test]
    async fn fresh_legacy_cached_error_is_removed_before_recomputation() {
        const SECRET: &str = "LEGACY_SECRET_SENTINEL";

        let cache = fresh_cache();
        let topic = cache.topic("test").expect("open test topic");
        let (updater, _) = topic
            .get_for_update("legacy-error")
            .await
            .expect("open legacy error for update");
        let legacy = CacheEntry::<u64> {
            expires: Utc::now() + Duration::from_secs(60),
            result: CacheResult::Err(SECRET.to_string()),
        };
        let legacy = serde_json::to_vec(&legacy).expect("serialize legacy cached error");
        updater
            .write(&legacy, Duration::from_secs(60))
            .expect("seed legacy cached error");

        let calls = AtomicUsize::new(0);
        let error = cache_get_inner(
            &cache,
            opts("legacy-error", Duration::from_secs(60)),
            async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<CacheComputeResult<u64>, _>(
                    NoCacheError(anyhow::anyhow!("CURRENT_TRANSIENT_FAILURE")).into(),
                )
            },
        )
        .await
        .expect_err("the recomputed transient failure must be returned");
        let rendered = format!("{error:#}");

        assert!(!rendered.contains(SECRET), "{rendered}");
        assert!(rendered.contains("CURRENT_TRANSIENT_FAILURE"), "{rendered}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        assert!(
            topic
                .get("legacy-error")
                .expect("read removed legacy cached error")
                .is_none(),
            "legacy error must be deleted when recomputation is not cacheable"
        );
    }

    #[tokio::test]
    async fn expired_legacy_cached_error_is_removed_before_no_cache_error() {
        const SECRET: &str = "EXPIRED_LEGACY_SECRET_SENTINEL";

        let cache = fresh_cache();
        let calls = AtomicUsize::new(0);
        let topic = cache.topic("test").expect("open test topic");
        let (updater, _) = topic
            .get_for_update("expired-legacy-error")
            .await
            .expect("open expired legacy error for update");
        let legacy = CacheEntry::<u64> {
            expires: Utc::now() - Duration::from_secs(1),
            result: CacheResult::Err(SECRET.to_string()),
        };
        let legacy = serde_json::to_vec(&legacy).expect("serialize expired legacy cached error");
        updater
            .write(&legacy, Duration::from_secs(60))
            .expect("seed expired legacy cached error");

        let error = cache_get_inner(
            &cache,
            opts("expired-legacy-error", Duration::from_secs(60)),
            async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<CacheComputeResult<u64>, _>(
                    NoCacheError(anyhow::anyhow!("CURRENT_TRANSIENT_FAILURE")).into(),
                )
            },
        )
        .await
        .expect_err("the current transient failure must be returned");
        let rendered = format!("{error:#}");

        assert!(error.downcast_ref::<NoCacheError>().is_some(), "{rendered}");
        assert!(!rendered.contains(SECRET), "{rendered}");
        assert!(rendered.contains("CURRENT_TRANSIENT_FAILURE"), "{rendered}");
        assert!(
            topic
                .get("expired-legacy-error")
                .expect("read removed expired legacy error")
                .is_none(),
            "expired legacy error must be deleted before returning NoCacheError"
        );

        cache_get_inner(
            &cache,
            opts("expired-legacy-error", Duration::from_secs(60)),
            async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<CacheComputeResult<u64>, _>(
                    NoCacheError(anyhow::anyhow!("SECOND_TRANSIENT_FAILURE")).into(),
                )
            },
        )
        .await
        .expect_err("the second transient failure must also be recomputed");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "removing a legacy error must preserve the no-cache retry contract"
        );
    }

    #[tokio::test]
    async fn malformed_cached_value_is_removed_before_no_cache_error() {
        const SECRET: &str = "MALFORMED_CACHE_SECRET_SENTINEL";

        let cache = fresh_cache();
        let topic = cache.topic("test").expect("open test topic");
        let (updater, _) = topic
            .get_for_update("malformed-error")
            .await
            .expect("open malformed error for update");
        updater
            .write(
                format!(r#"{{"result":{{"Err":"{SECRET}"}}}}"#).as_bytes(),
                Duration::from_secs(60),
            )
            .expect("seed malformed cached error");

        cache_get_inner(
            &cache,
            opts("malformed-error", Duration::from_secs(60)),
            async {
                Err::<CacheComputeResult<u64>, _>(
                    NoCacheError(anyhow::anyhow!("CURRENT_TRANSIENT_FAILURE")).into(),
                )
            },
        )
        .await
        .expect_err("the current transient failure must be returned");

        assert!(
            topic
                .get("malformed-error")
                .expect("read removed malformed cached value")
                .is_none(),
            "malformed cache data must be deleted before returning NoCacheError"
        );
    }

    /// The load-bearing test for the bug-fix: a future returning a NoCacheError
    /// MUST cause cache_get to skip the negative-cache write entirely. The next
    /// call must re-execute the future, not return a cached failure. This is
    /// what lets a 2FA verification code retry succeed inside its 15-min
    /// validity window.
    #[tokio::test]
    async fn no_cache_error_skips_negative_cache_so_future_re_executes() {
        let cache = fresh_cache();
        let calls = AtomicUsize::new(0);

        let f1 = async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err::<CacheComputeResult<u64>, _>(NoCacheError(anyhow::anyhow!("transient")).into())
        };
        let r1 = cache_get_inner(&cache, opts("nocache", Duration::from_secs(60)), f1).await;

        let f2 = async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err::<CacheComputeResult<u64>, _>(NoCacheError(anyhow::anyhow!("transient")).into())
        };
        let r2 = cache_get_inner(&cache, opts("nocache", Duration::from_secs(60)), f2).await;

        assert!(r1.is_err());
        assert!(r2.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "NoCacheError must NOT be cached; second call MUST re-execute the future"
        );
    }

    #[tokio::test]
    async fn no_cache_error_message_propagates_and_downcast_survives() {
        let cache = fresh_cache();
        let f = async {
            Err::<CacheComputeResult<u64>, _>(
                NoCacheError(anyhow::anyhow!("specific message")).into(),
            )
        };
        let err = cache_get_inner(&cache, opts("msg", Duration::from_secs(60)), f)
            .await
            .unwrap_err();

        assert!(
            format!("{err:#}").contains("specific message"),
            "error message lost: got {err:#}"
        );
        assert!(
            err.downcast_ref::<NoCacheError>().is_some(),
            "downcast must survive cache_get's pass-through"
        );
    }

    /// Regression guard: existing successful cache entries must still be
    /// served on subsequent calls regardless of whether subsequent futures
    /// would have returned NoCacheError. Just confirms cache_get's read path
    /// is independent of the future type.
    #[tokio::test]
    async fn fresh_ok_entry_is_returned_without_running_future() {
        let cache = fresh_cache();
        let calls = AtomicUsize::new(0);

        let f1 = async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, anyhow::Error>(CacheComputeResult::WithTtl(
                "hello".to_string(),
                Duration::from_secs(30),
            ))
        };
        let _ = cache_get_inner(&cache, opts("ttl-ok", Duration::from_secs(60)), f1)
            .await
            .unwrap();

        // Second call: future would NoCacheError, but cache should not run it.
        let f2 = async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err::<CacheComputeResult<String>, _>(
                NoCacheError(anyhow::anyhow!("would bypass")).into(),
            )
        };
        let v2 = cache_get_inner(&cache, opts("ttl-ok", Duration::from_secs(60)), f2)
            .await
            .unwrap();
        assert_eq!(v2, "hello");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
